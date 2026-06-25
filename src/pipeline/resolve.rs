//! Lock phase: resolve the manifest into an updated lockfile. May access
//! the network. Does not touch any skills directory.

use std::path::{Path, PathBuf};

use crate::error::{ExitCode, Result, SkmError};
use crate::model::agent::{Agent, parse_agents};
use crate::model::config::Scope;
use crate::model::lockfile::{self, LockSkill, LockSource, Lockfile};
use crate::model::manifest::Manifest;
use crate::source::{self, ResolvedSource, Source};
use crate::sys::cache::{self, Cache};
use crate::sys::fsutil;
use crate::sys::hash::{self, Sha256};
use crate::ui;

/// Which entries `--upgrade` applies to.
#[derive(Debug, Clone)]
pub enum Upgrade {
    None,
    All,
    Named(Vec<String>),
}

impl Upgrade {
    fn applies(&self, name: &str) -> bool {
        match self {
            Upgrade::None => false,
            Upgrade::All => true,
            Upgrade::Named(names) => names.iter().any(|n| n == name),
        }
    }
}

pub struct LockOptions {
    pub upgrade: Upgrade,
    pub offline: bool,
    /// Under `--dry-run`, a source that needs the network to re-resolve while
    /// `--offline` is deferred (flagged "may update") instead of erroring.
    pub dry_run: bool,
}

/// Whether a source is an immutable pin (git commit SHA) — `--upgrade` is a
/// no-op for it. Delegates to [`crate::source::git::is_commit_sha`] so the
/// hex-format rule has a single definition.
fn is_immutable_pin(source: &Source) -> bool {
    matches!(source, Source::Git { ref_: Some(r), .. } if crate::source::git::is_commit_sha(r))
}

/// Compare manifest source identity against an existing lock entry.
/// Returns true if unchanged (re-resolution can be skipped).
pub fn identity_matches(
    manifest_source: &Source,
    manifest_dir: &Path,
    lock: &LockSource,
) -> Result<bool> {
    Ok(match (manifest_source, lock) {
        (
            Source::Git { repo, ref_, subdir },
            LockSource::Git {
                repo: lrepo,
                ref_: lref,
                subdir: lsubdir,
                ..
            },
        ) => {
            cache::normalize_url(repo) == cache::normalize_url(lrepo)
                && ref_ == lref
                && subdir == lsubdir
        }
        (
            Source::Tar {
                url,
                subdir,
                sha256,
            },
            LockSource::Tar {
                url: lurl,
                subdir: lsubdir,
                archive_sha256,
            },
        ) => sha_url_match(url, subdir, sha256, lurl, lsubdir, *archive_sha256),
        (
            Source::Zip {
                url,
                subdir,
                sha256,
            },
            LockSource::Zip {
                url: lurl,
                subdir: lsubdir,
                archive_sha256,
            },
        ) => sha_url_match(url, subdir, sha256, lurl, lsubdir, *archive_sha256),
        (Source::Local { path }, LockSource::Local { path: lpath }) => {
            let a = source::local::normalize_local(manifest_dir, path)?;
            let b = source::local::normalize_local(manifest_dir, lpath)?;
            a == b
        }
        _ => false,
    })
}

fn sha_url_match(
    url: &str,
    subdir: &Option<String>,
    declared: &Option<String>,
    lurl: &str,
    lsubdir: &Option<String>,
    archive_sha256: Sha256,
) -> bool {
    if cache::normalize_url(url) != cache::normalize_url(lurl) || subdir != lsubdir {
        return false;
    }
    match declared {
        // A declared sha256 must match the recorded artifact, else identity
        // changed and a relock (with verification) is triggered.
        Some(d) => *d == archive_sha256.to_hex(),
        None => true,
    }
}

fn make_lock_source(source: &Source, resolved: &ResolvedSource) -> Result<LockSource> {
    Ok(match (source, resolved) {
        (Source::Git { repo, ref_, subdir }, ResolvedSource::Git { commit, ref_: rref }) => {
            LockSource::Git {
                repo: repo.clone(),
                ref_: ref_.clone(),
                subdir: subdir.clone(),
                resolved_ref: rref.clone(),
                resolved_commit: commit.clone(),
            }
        }
        (Source::Tar { url, subdir, .. }, ResolvedSource::Archive { sha256 }) => LockSource::Tar {
            url: url.clone(),
            subdir: subdir.clone(),
            archive_sha256: *sha256,
        },
        (Source::Zip { url, subdir, .. }, ResolvedSource::Archive { sha256 }) => LockSource::Zip {
            url: url.clone(),
            subdir: subdir.clone(),
            archive_sha256: *sha256,
        },
        (Source::Local { path }, ResolvedSource::Local) => LockSource::Local { path: path.clone() },
        _ => {
            return Err(SkmError::general(
                "internal: source/resolved mismatch in make_lock_source",
            ));
        }
    })
}

pub fn resolved_from_lock(lock: &LockSource) -> ResolvedSource {
    match lock {
        LockSource::Git {
            resolved_ref,
            resolved_commit,
            ..
        } => ResolvedSource::Git {
            commit: resolved_commit.clone(),
            ref_: resolved_ref.clone(),
        },
        LockSource::Tar { archive_sha256, .. } | LockSource::Zip { archive_sha256, .. } => {
            ResolvedSource::Archive {
                sha256: *archive_sha256,
            }
        }
        LockSource::Local { .. } => ResolvedSource::Local,
    }
}

/// Run the lock phase, returning the new lockfile, whether it differs
/// substantively from `existing` (write timing), and the names deferred
/// under `--dry-run --offline` (their source identity changed but the network
/// is unavailable to re-resolve).
pub fn lock_phase(
    manifest: &Manifest,
    manifest_dir: &Path,
    scope: &Scope,
    existing: &Lockfile,
    cache: &Cache,
    opts: &LockOptions,
) -> Result<(Lockfile, bool, Vec<String>)> {
    let mut new_lock = Lockfile::empty();
    let mut deferred: Vec<String> = Vec::new();

    // Skills roots for the local-containment check.
    let roots: Vec<PathBuf> = Agent::ALL
        .iter()
        .filter_map(|a| a.skills_root(scope).ok())
        .collect();

    for skill in &manifest.skills {
        // Ensure agents resolve to a non-empty, *valid* set: values must be
        // known Agents, even though agents are never written to the lock.
        let ids = skill.effective_agents(&manifest.default_agents)?;
        parse_agents(&ids)?;

        // local must not lie inside any skills_root: a source that is
        // a subtree of its own agent would copy a directory into itself
        // on sync (recursive engulfment). Lexical prefix check, no symlink resolve.
        if let Source::Local { path } = &skill.source {
            let abs = source::local::normalize_local(manifest_dir, path)?;
            for root in &roots {
                if abs == *root || abs.starts_with(root) {
                    return Err(SkmError::general(format!(
                        "error: local source '{path}' lies inside a skills_root ('{}'); pick a path outside any agent",
                        root.display()
                    )));
                }
            }
        }

        let existing_entry = existing.get(&skill.name);
        let identity_same = match existing_entry {
            Some(e) => identity_matches(&skill.source, manifest_dir, &e.source)?,
            None => false,
        };

        let upgrade_here = opts.upgrade.applies(&skill.name) && !is_immutable_pin(&skill.source);
        let re_resolve = existing_entry.is_none() || !identity_same || upgrade_here;

        // Obtain resolved pointers (network only when re-resolving).
        let resolved = if re_resolve {
            // Announce the (likely networked) work; local sources stay quiet.
            if let Some(label) = skill.source.progress_label() {
                ui::activity!("Resolving {} ({label})…", skill.name);
            }
            let prev_ref = existing_entry.and_then(|e| match &e.source {
                LockSource::Git { resolved_ref, .. } => resolved_ref.as_deref(),
                _ => None,
            });
            match skill
                .source
                .resolve(cache, manifest_dir, opts.offline, upgrade_here, prev_ref)
            {
                Ok(r) => r,
                // Offline dry-run: the source needs the network to re-resolve but
                // we are offline. Flag it as "may update" (--offline + --dry-run)
                // and carry the existing lock entry forward unchanged (a brand-new
                // skill simply has no entry yet). The real online run resolves here.
                Err(e)
                    if opts.offline
                        && opts.dry_run
                        && matches!(e.exit, ExitCode::Network | ExitCode::LockMissing) =>
                {
                    deferred.push(skill.name.clone());
                    if let Some(en) = existing_entry {
                        new_lock.skills.push(en.clone());
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }
        } else {
            let entry = existing_entry.as_ref().ok_or_else(|| {
                SkmError::general("internal: re-resolve false but no existing entry")
            })?;
            resolved_from_lock(&entry.source)
        };

        // Always materialize + SKILL.md check (live) + hash (step 3).
        let staging_base = cache.new_staging(&skill.name)?;
        let result = (|| {
            // Lock phase never runs under --frozen/--locked, so the
            // frozen_lock_context flag is always false here.
            let content_root = skill.source.materialize(
                cache,
                manifest_dir,
                &resolved,
                opts.offline,
                false,
                &staging_base,
            )?;
            if !source::has_skill_md(&content_root) {
                let mut msg = format!(
                    "error: skill '{}' content root has no SKILL.md\n  → not a valid skill artifact",
                    skill.name
                );
                if matches!(
                    skill.source,
                    Source::Git { .. } | Source::Zip { .. } | Source::Tar { .. }
                ) && skill.source.subdir().is_none()
                {
                    // Top-level-directory pitfall: when the archive
                    // contains exactly one top-level directory and that
                    // directory holds SKILL.md, suggest the specific
                    // `--subdir <name>` value.
                    match single_top_skill_dir(&content_root) {
                        Some(name) => msg.push_str(&format!(
                            "\n  → The skill appears to be in subdirectory '{name}/'; try --subdir {name}"
                        )),
                        None => msg.push_str(
                            "\n  → If the skill is in a subdirectory, use --subdir to specify it.",
                        ),
                    }
                }
                return Err(SkmError::general(msg));
            }
            let content_sha256 =
                hash::hash_tree(&content_root, skill.source.source_type().exec_policy())?;
            Ok(content_sha256)
        })();
        let _ = fsutil::remove_dir_all(&staging_base);
        let content_sha256 = result?;

        let lock_source = make_lock_source(&skill.source, &resolved)?;
        new_lock.skills.push(LockSkill {
            name: skill.name.clone(),
            content_sha256,
            source: lock_source,
        });
    }

    new_lock.sort();
    let changed = !lockfile::content_eq(existing, &new_lock);
    Ok((new_lock, changed, deferred))
}

/// Resolve a single skill: network resolve → materialize → SKILL.md check →
/// hash → LockSkill. Used by `add` to validate a skill **before** mutating the
/// manifest, avoiding the "write broken manifest entry then fail later" problem.
pub fn resolve_single_skill(
    manifest_dir: &Path,
    scope: &Scope,
    name: &str,
    source: &Source,
    cache: &Cache,
    offline: bool,
) -> Result<LockSkill> {
    // local containment check
    if let Source::Local { path } = source {
        let roots: Vec<PathBuf> = Agent::ALL
            .iter()
            .filter_map(|a| a.skills_root(scope).ok())
            .collect();
        let abs = source::local::normalize_local(manifest_dir, path)?;
        for root in &roots {
            if abs == *root || abs.starts_with(root) {
                return Err(SkmError::general(format!(
                    "error: local source '{path}' lies inside a skills_root ('{}'); pick a path outside any agent",
                    root.display()
                )));
            }
        }
    }

    // Announce the (likely networked) work; local sources stay quiet.
    if let Some(label) = source.progress_label() {
        ui::activity!("Resolving {name} ({label})…");
    }
    // Resolve over the network (or from local fs for local sources).
    let resolved = source.resolve(cache, manifest_dir, offline, false, None)?;

    // Materialize + SKILL.md check (live, step 3) + hash.
    let staging_base = cache.new_staging(name)?;
    let content_sha256 = (|| {
        let content_root = source.materialize(
            cache,
            manifest_dir,
            &resolved,
            offline,
            false,
            &staging_base,
        )?;
        if !source::has_skill_md(&content_root) {
            let mut msg = format!(
                "error: skill '{name}' content root has no SKILL.md\n  → not a valid skill artifact"
            );
            if matches!(
                source,
                Source::Git { .. } | Source::Zip { .. } | Source::Tar { .. }
            ) && source.subdir().is_none()
            {
                match single_top_skill_dir(&content_root) {
                    Some(dir) => msg.push_str(&format!(
                        "\n  → The skill appears to be in subdirectory '{dir}/'; try --subdir {dir}"
                    )),
                    None => msg.push_str(
                        "\n  → If the skill is in a subdirectory, use --subdir to specify it.",
                    ),
                }
            }
            return Err(SkmError::general(msg));
        }
        hash::hash_tree(&content_root, source.source_type().exec_policy())
    })();
    let _ = fsutil::remove_dir_all(&staging_base);
    let content_sha256 = content_sha256?;

    let lock_source = make_lock_source(source, &resolved)?;
    Ok(LockSkill {
        name: name.to_string(),
        content_sha256,
        source: lock_source,
    })
}

/// If `content_root` has exactly one direct child entry, and that entry is a
/// directory containing `SKILL.md`, return its name. Used to suggest a precise
/// `--subdir` value for the top-level-directory pitfall.
fn single_top_skill_dir(content_root: &Path) -> Option<String> {
    let mut entries = std::fs::read_dir(content_root).ok()?;
    let first = entries.next()?.ok()?;
    if entries.next().is_some() {
        return None;
    }
    if !first.file_type().ok()?.is_dir() {
        return None;
    }
    if !first.path().join("SKILL.md").is_file() {
        return None;
    }
    let name = first.file_name().to_string_lossy().to_string();
    source::validate_source_subdir(&name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn git_lock(repo: &str, ref_: Option<&str>, subdir: Option<&str>) -> LockSource {
        LockSource::Git {
            repo: repo.to_string(),
            ref_: ref_.map(str::to_string),
            subdir: subdir.map(str::to_string),
            resolved_ref: ref_.map(str::to_string),
            resolved_commit: "0".repeat(40),
        }
    }

    #[test]
    fn git_ref_some_vs_none_changes_identity() {
        // ref "main" → omitted flips "fixed branch" ↔ "follow default" →
        // different identity (relock).
        let md = Path::new("/tmp");
        let pinned = Source::Git {
            repo: "https://x/r".into(),
            ref_: Some("main".into()),
            subdir: None,
        };
        let follow = Source::Git {
            repo: "https://x/r".into(),
            ref_: None,
            subdir: None,
        };
        let lock_main = git_lock("https://x/r", Some("main"), None);
        assert!(identity_matches(&pinned, md, &lock_main).unwrap());
        assert!(!identity_matches(&follow, md, &lock_main).unwrap());
    }

    #[test]
    fn git_url_normalized_for_identity() {
        // `.git` suffix + trailing slash stripped before comparison.
        let md = Path::new("/tmp");
        let src = Source::Git {
            repo: "https://x/r.git/".into(),
            ref_: Some("main".into()),
            subdir: None,
        };
        let lock = git_lock("https://x/r", Some("main"), None);
        assert!(identity_matches(&src, md, &lock).unwrap());
    }

    #[test]
    fn tar_declared_sha256_gates_identity() {
        // a declared sha256 must equal the recorded archive_sha256;
        // None matches anything; a mismatch triggers relock.
        let md = Path::new("/tmp");
        let sha_a = "a".repeat(64);
        let sha_b = "b".repeat(64);
        let lock = LockSource::Tar {
            url: "https://x/a.tar.gz".into(),
            subdir: None,
            archive_sha256: Sha256::from_hex(&sha_a).unwrap(),
        };
        let tar = |sha: Option<&str>| Source::Tar {
            url: "https://x/a.tar.gz".into(),
            subdir: None,
            sha256: sha.map(str::to_string),
        };
        assert!(identity_matches(&tar(Some(&sha_a)), md, &lock).unwrap());
        assert!(!identity_matches(&tar(Some(&sha_b)), md, &lock).unwrap());
        assert!(identity_matches(&tar(None), md, &lock).unwrap());
    }

    #[test]
    fn local_equivalent_spellings_match() {
        // `./vendor/x` and `vendor/x` normalize equal under the same base.
        let md = Path::new("/home/u/proj");
        let src = Source::Local {
            path: "./vendor/x".into(),
        };
        let lock = LockSource::Local {
            path: "vendor/x".into(),
        };
        assert!(identity_matches(&src, md, &lock).unwrap());
    }

    #[test]
    fn cross_type_never_matches() {
        let md = Path::new("/tmp");
        let src = Source::Local { path: "./v".into() };
        let lock = git_lock("https://x/r", Some("main"), None);
        assert!(!identity_matches(&src, md, &lock).unwrap());
    }

    #[test]
    fn is_immutable_pin_only_for_commit_sha() {
        // a 40-hex lowercase commit is an immutable pin; branch/tag/short or
        // uppercase forms are not.
        let pin = Source::Git {
            repo: "r".into(),
            ref_: Some("a".repeat(40)),
            subdir: None,
        };
        assert!(is_immutable_pin(&pin));
        let branch = Source::Git {
            repo: "r".into(),
            ref_: Some("main".into()),
            subdir: None,
        };
        assert!(!is_immutable_pin(&branch));
        let upper = Source::Git {
            repo: "r".into(),
            ref_: Some("A".repeat(40)),
            subdir: None,
        };
        assert!(!is_immutable_pin(&upper));
        let none = Source::Git {
            repo: "r".into(),
            ref_: None,
            subdir: None,
        };
        assert!(!is_immutable_pin(&none));
    }
}
