//! Source abstraction: the four locator kinds and their resolve/materialize
//! behavior. `SourceType` is the canonical source-kind enum used
//! across hashing, manifest, and lockfile.

pub mod git;
pub mod local;
pub mod tar;
pub mod zip;

use std::path::{Path, PathBuf};

use crate::error::{Result, SkmError};
use crate::sys::cache::Cache;
use crate::sys::hash::Sha256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Git,
    Tar,
    Zip,
    Local,
}

impl SourceType {
    /// Map to the exec-bit policy used by content hashing.  Git and tar
    /// carry the exec bit from the archive; zip and local ignore it.
    pub fn exec_policy(self) -> crate::sys::hash::ExecPolicy {
        match self {
            SourceType::Git | SourceType::Tar => crate::sys::hash::ExecPolicy::Include,
            SourceType::Zip | SourceType::Local => crate::sys::hash::ExecPolicy::Exclude,
        }
    }
}

/// A fully-typed source as derived from the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Git {
        repo: String,
        ref_: Option<String>,
        subdir: Option<String>,
    },
    Tar {
        url: String,
        subdir: Option<String>,
        sha256: Option<String>,
    },
    Zip {
        url: String,
        subdir: Option<String>,
        sha256: Option<String>,
    },
    Local {
        /// Original manifest spelling (preserved in the lock).
        path: String,
    },
}

impl Source {
    pub fn source_type(&self) -> SourceType {
        match self {
            Source::Git { .. } => SourceType::Git,
            Source::Tar { .. } => SourceType::Tar,
            Source::Zip { .. } => SourceType::Zip,
            Source::Local { .. } => SourceType::Local,
        }
    }

    pub fn subdir(&self) -> Option<&str> {
        match self {
            Source::Git { subdir, .. }
            | Source::Tar { subdir, .. }
            | Source::Zip { subdir, .. } => subdir.as_deref(),
            Source::Local { .. } => None,
        }
    }

    /// One-line, human-facing summary of the *declared* source (no resolved
    /// commit/digest — that lives in the lock). Used by `skm status` for skills
    /// not yet in the lock; see [`crate::model::lockfile::LockSource::summary`]
    /// for the resolved counterpart.
    pub fn summary(&self) -> String {
        let with_subdir = |kind: &str, locator: &str, subdir: &Option<String>| {
            let mut s = format!("{kind} {locator}");
            if let Some(sd) = subdir {
                s.push_str(&format!(" [subdir: {sd}]"));
            }
            s
        };
        match self {
            Source::Git { repo, ref_, subdir } => {
                let mut s = format!("git {repo}");
                if let Some(r) = ref_ {
                    s.push_str(&format!(" @ {r}"));
                }
                if let Some(sd) = subdir {
                    s.push_str(&format!(" [subdir: {sd}]"));
                }
                s
            }
            Source::Tar { url, subdir, .. } => with_subdir("tar", url, subdir),
            Source::Zip { url, subdir, .. } => with_subdir("zip", url, subdir),
            Source::Local { path } => format!("local {path}"),
        }
    }

    /// A short `<kind> <locator>` label for a progress line, or `None` for
    /// sources that touch no network (local). Used to announce in-flight
    /// resolve/fetch work; see [`crate::ui::activity`].
    pub fn progress_label(&self) -> Option<String> {
        match self {
            Source::Git { repo, .. } => Some(format!("git {repo}")),
            Source::Tar { url, .. } => Some(format!("tar {url}")),
            Source::Zip { url, .. } => Some(format!("zip {url}")),
            Source::Local { .. } => None,
        }
    }

    /// Resolve the source: network access as needed. Produces the immutable
    /// pointers recorded in the lock.
    pub fn resolve(
        &self,
        cache: &Cache,
        manifest_dir: &Path,
        offline: bool,
        upgrade: bool,
        lock_resolved_ref: Option<&str>,
    ) -> Result<ResolvedSource> {
        match self {
            Source::Git {
                repo,
                ref_,
                subdir: _,
            } => git::resolve(
                cache,
                repo,
                ref_.as_deref(),
                offline,
                upgrade,
                lock_resolved_ref,
            ),
            Source::Tar { url, sha256, .. } => tar::resolve(cache, url, sha256.as_deref(), offline),
            Source::Zip { url, sha256, .. } => zip::resolve(cache, url, sha256.as_deref(), offline),
            Source::Local { path } => {
                // Validate existence/readability; no network, no cache.
                local::validate(manifest_dir, path)?;
                Ok(ResolvedSource::Local)
            }
        }
    }

    /// Materialize the content tree into `staging_base`, returning the content
    /// root path (a subdirectory of `staging_base`, or the local copy). The
    /// `subdir` prefix is stripped.
    ///
    /// `frozen_lock_context` reflects "the lock requires this artifact" — when
    /// true, an offline cache miss maps to [`ExitCode::LockMissing`] (6)
    /// instead of [`ExitCode::Network`] (4) per the `--offline` table.
    pub fn materialize(
        &self,
        cache: &Cache,
        manifest_dir: &Path,
        resolved: &ResolvedSource,
        offline: bool,
        frozen_lock_context: bool,
        staging_base: &Path,
    ) -> Result<PathBuf> {
        match (self, resolved) {
            (Source::Git { repo, subdir, .. }, ResolvedSource::Git { commit, .. }) => {
                git::materialize(
                    cache,
                    repo,
                    commit,
                    subdir.as_deref(),
                    offline,
                    frozen_lock_context,
                    staging_base,
                )
            }
            (Source::Tar { url, subdir, .. }, ResolvedSource::Archive { sha256 }) => {
                tar::materialize(
                    cache,
                    url,
                    *sha256,
                    subdir.as_deref(),
                    offline,
                    frozen_lock_context,
                    staging_base,
                )
            }
            (Source::Zip { url, subdir, .. }, ResolvedSource::Archive { sha256 }) => {
                zip::materialize(
                    cache,
                    url,
                    *sha256,
                    subdir.as_deref(),
                    offline,
                    frozen_lock_context,
                    staging_base,
                )
            }
            (Source::Local { path }, ResolvedSource::Local) => {
                local::materialize(manifest_dir, path, staging_base)
            }
            _ => Err(SkmError::general(
                "internal: source/resolved mismatch in materialize",
            )),
        }
    }
}

/// Immutable pointers produced by [`Source::resolve`].
#[derive(Debug, Clone)]
pub enum ResolvedSource {
    /// Git: pinned commit + optional symbolic ref.
    Git {
        commit: String,
        ref_: Option<String>,
    },
    /// Tar/zip: content-addressed archive digest.
    Archive { sha256: Sha256 },
    /// Local: no resolution metadata needed.
    Local,
}

/// Validate a `subdir` field for git/zip/tar. Returns the
/// normalized relative subdir (forward slashes, no leading `./`).
pub fn validate_source_subdir(subdir: &str) -> Result<String> {
    let p = Path::new(subdir);
    if p.is_absolute() {
        return Err(SkmError::general(format!(
            "subdir '{subdir}' must be relative, not absolute"
        )));
    }
    let mut parts: Vec<String> = Vec::new();
    for comp in p.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => {
                return Err(SkmError::general(format!(
                    "subdir '{subdir}' must not contain '..'"
                )));
            }
            Normal(s) => parts.push(s.to_string_lossy().to_string()),
            RootDir | Prefix(_) => {
                return Err(SkmError::general(format!(
                    "subdir '{subdir}' is not relative"
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

/// Whether the content tree root contains `SKILL.md`.
pub fn has_skill_md(content_root: &Path) -> bool {
    content_root.join("SKILL.md").is_file()
}

/// Given a freshly extracted archive root and an optional `subdir`,
/// return the content tree root. Validates the subdir exists.
pub fn content_root_with_subdir(extract_root: &Path, subdir: Option<&str>) -> Result<PathBuf> {
    match subdir {
        None => Ok(extract_root.to_path_buf()),
        Some(sd) => {
            let norm = validate_source_subdir(sd)?;
            let candidate = tar::safe_join(extract_root, Path::new(&norm))?;
            if !candidate.is_dir() {
                return Err(SkmError::general(format!(
                    "subdir '{sd}' does not point to a directory inside the source"
                )));
            }
            Ok(candidate)
        }
    }
}
