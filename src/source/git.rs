//! Git source: resolve via the system `git` CLI, materialize via `git archive`
//! into a bare mirror cache. Submodules are not supported (v1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{ExitCode, Result, SkmError, three_part};
use crate::sys::cache::Cache;
use crate::ui;

use super::ResolvedSource;

/// Return true if `s` is a 40-char lowercase hex commit SHA
/// (form detection: `[0-9a-f]{40}`).
pub(crate) fn is_commit_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Ensure `git` is available in PATH.
pub fn ensure_git() -> Result<()> {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|_| ())
        .map_err(|_| {
            SkmError::general(three_part(
                "error: git not found",
                "the `git` executable is not in PATH",
                "Install git and ensure it is on your PATH.",
            ))
        })
}

/// Prevent git from prompting for credentials interactively (terminal or GUI).
/// Call this on any `Command` that contacts a remote.
///
/// Why it matters: GitHub returns 401 (not 404) for a missing/private repo, so
/// without this git would block on a credential prompt — hanging CI forever.
/// Fail fast instead; users configure a non-interactive helper (see README).
fn non_interactive(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null())
}

struct RemoteInfo {
    default_branch: Option<String>,
    heads: HashMap<String, String>,
    tags: HashMap<String, String>,
}

fn ls_remote_symref(repo: &str) -> Result<RemoteInfo> {
    let out = non_interactive(&mut Command::new("git"))
        .args(["ls-remote", "--symref", repo])
        .output()
        .map_err(|e| SkmError::network(format!("git ls-remote failed to start: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr_trimmed = stderr.trim();

        // Detect auth-related failures: when git cannot get credentials (repo
        // is private, doesn't exist, or credential helper isn't configured),
        // give a targeted hint instead of the generic network-troubleshooting
        // suggestion.
        let hint = if stderr_trimmed.contains("terminal prompts disabled")
            || stderr_trimmed.contains("could not read Username")
        {
            "This repo requires authentication.\nFor private repos, configure a non-interactive credential helper\n  (e.g. `gh auth setup-git`, the system keychain, or set GIT_ASKPASS).\nIf the repo does not exist, check the URL."
        } else {
            "Check the URL, your network, and git credentials (SSH agent / credential helper)."
        };

        return Err(SkmError::network(three_part(
            &format!("error: cannot reach git remote '{repo}'"),
            stderr_trimmed,
            hint,
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut info = RemoteInfo {
        default_branch: None,
        heads: HashMap::new(),
        tags: HashMap::new(),
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("ref: ") {
            // "ref: refs/heads/main\tHEAD"
            if let Some((target, name)) = rest.split_once('\t') {
                if name.trim() == "HEAD" {
                    if let Some(b) = target.trim().strip_prefix("refs/heads/") {
                        info.default_branch = Some(b.to_string());
                    }
                }
            }
            continue;
        }
        if let Some((sha, refname)) = line.split_once('\t') {
            let sha = sha.trim().to_string();
            let refname = refname.trim();
            if let Some(tag) = refname.strip_prefix("refs/tags/") {
                // Peeled tag ("...^{}") takes precedence over the tag object.
                if let Some(base) = tag.strip_suffix("^{}") {
                    info.tags.insert(base.to_string(), sha);
                } else {
                    info.tags.entry(tag.to_string()).or_insert(sha);
                }
            } else if let Some(head) = refname.strip_prefix("refs/heads/") {
                info.heads.insert(head.to_string(), sha);
            }
        }
    }
    Ok(info)
}

/// Resolve a git source to a commit + symbolic ref.
pub fn resolve(
    cache: &Cache,
    repo: &str,
    ref_: Option<&str>,
    offline: bool,
    upgrade: bool,
    lock_resolved_ref: Option<&str>,
) -> Result<ResolvedSource> {
    ensure_git()?;
    let _ = cache; // git resolves over the network; the cache is used at materialize time

    // Commit SHA pin: immutable, no symbolic ref.
    if let Some(r) = ref_ {
        if is_commit_sha(r) {
            return Ok(ResolvedSource::Git {
                commit: r.to_string(),
                ref_: None,
            });
        }
    }

    if offline {
        return Err(SkmError::network(three_part(
            "error: cannot resolve git source while offline",
            "resolving a branch/tag/default ref requires a network ls-remote",
            "Run without --offline, or pin an exact commit SHA in skm.toml.",
        )));
    }

    let info = ls_remote_symref(repo)?;

    let (resolved_ref, commit) = match ref_ {
        None => {
            // Follow remote default branch. Handle drift on --upgrade.
            match info.default_branch.clone() {
                Some(def) => {
                    if upgrade {
                        if let Some(prev) = lock_resolved_ref {
                            if prev != def && info.heads.contains_key(prev) {
                                return Err(SkmError::general(format!(
                                    "error: remote default branch changed from '{prev}' to '{def}'\n  → The remote's HEAD now points elsewhere.\n  → Add 'ref = \"{def}\"' to skm.toml to explicitly pin the branch."
                                )));
                            }
                        }
                    }
                    let commit = info.heads.get(&def).cloned().ok_or_else(|| {
                        SkmError::network(format!(
                            "could not resolve default branch '{def}' of '{repo}'"
                        ))
                    })?;
                    (Some(def), commit)
                }
                None => {
                    // Server gave no symref HEAD: degrade to lock's resolved_ref.
                    if let Some(prev) = lock_resolved_ref {
                        if let Some(commit) = info.heads.get(prev).cloned() {
                            ui::warn!(
                                "remote default branch could not be determined; following recorded ref '{prev}'.\n  → Specify 'ref' in skm.toml to remove this ambiguity."
                            );
                            (Some(prev.to_string()), commit)
                        } else {
                            return Err(SkmError::network(format!(
                                "cannot determine default branch of '{repo}' and recorded ref '{prev}' is gone"
                            )));
                        }
                    } else {
                        return Err(SkmError::network(three_part(
                            &format!("error: cannot determine default branch of '{repo}'"),
                            "the remote did not advertise a symbolic HEAD",
                            "Specify 'ref' in skm.toml to pin a branch or tag.",
                        )));
                    }
                }
            }
        }
        Some(r) => {
            // Tag takes precedence over a same-named branch (warn on conflict).
            if let Some(sha) = info.tags.get(r) {
                if info.heads.contains_key(r) {
                    ui::warn!("ref '{r}' matches both a tag and a branch; using the tag.");
                }
                (Some(r.to_string()), sha.clone())
            } else if let Some(sha) = info.heads.get(r) {
                (Some(r.to_string()), sha.clone())
            } else {
                return Err(SkmError::network(format!(
                    "ref '{r}' not found in remote '{repo}' (not a branch or tag)"
                )));
            }
        }
    };

    Ok(ResolvedSource::Git {
        commit,
        ref_: resolved_ref,
    })
}

fn mirror_has_commit(mirror: &Path, commit: &str) -> bool {
    Command::new("git")
        .args(["--git-dir"])
        .arg(mirror)
        .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn fetch_mirror(mirror: &Path, repo: &str) -> Result<()> {
    if mirror.join("HEAD").exists() || mirror.join("config").exists() {
        let out = non_interactive(&mut Command::new("git"))
            .args(["--git-dir"])
            .arg(mirror)
            .args([
                "fetch",
                "--prune",
                "--tags",
                repo,
                "+refs/heads/*:refs/heads/*",
                "+refs/tags/*:refs/tags/*",
            ])
            .output()
            .map_err(|e| SkmError::network(format!("git fetch failed to start: {e}")))?;
        if !out.status.success() {
            return Err(SkmError::network(three_part(
                &format!("error: git fetch failed for '{repo}'"),
                String::from_utf8_lossy(&out.stderr).trim(),
                "Check your network and git credentials.",
            )));
        }
    } else {
        if let Some(parent) = mirror.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = non_interactive(&mut Command::new("git"))
            .args(["clone", "--bare", repo])
            .arg(mirror)
            .output()
            .map_err(|e| SkmError::network(format!("git clone failed to start: {e}")))?;
        if !out.status.success() {
            return Err(SkmError::network(three_part(
                &format!("error: git clone failed for '{repo}'"),
                String::from_utf8_lossy(&out.stderr).trim(),
                "Check the URL, your network, and git credentials.",
            )));
        }
    }
    Ok(())
}

/// Ensure the bare mirror contains `commit`, fetching if needed.
///
/// `frozen_lock_context` causes an offline cache miss to report
/// [`ExitCode::LockMissing`] (6) instead of [`ExitCode::Network`] (4) — per the
/// `--offline` table.
fn ensure_mirror_commit(
    cache: &Cache,
    repo: &str,
    commit: &str,
    offline: bool,
    frozen_lock_context: bool,
) -> Result<PathBuf> {
    let mirror = cache.git_mirror(repo);
    if mirror_has_commit(&mirror, commit) {
        return Ok(mirror);
    }
    if offline {
        let exit = if frozen_lock_context {
            ExitCode::LockMissing
        } else {
            ExitCode::Network
        };
        return Err(SkmError::new(
            exit,
            three_part(
                &format!("error: commit {commit} not in cache and --offline given"),
                "the required git object is missing from the local mirror",
                "Run without --offline to fetch it.",
            ),
        ));
    }
    fetch_mirror(&mirror, repo)?;
    if !mirror_has_commit(&mirror, commit) {
        return Err(SkmError::network(format!(
            "commit {commit} not found in '{repo}' after fetch"
        )));
    }
    Ok(mirror)
}

/// Materialize via `git archive <commit>[:subdir]` piped into the tar extractor.
pub fn materialize(
    cache: &Cache,
    repo: &str,
    commit: &str,
    subdir: Option<&str>,
    offline: bool,
    frozen_lock_context: bool,
    staging_base: &Path,
) -> Result<PathBuf> {
    ensure_git()?;
    let _guard = cache.lock_shared()?;
    let mirror = ensure_mirror_commit(cache, repo, commit, offline, frozen_lock_context)?;

    let treeish = match subdir {
        Some(sd) => {
            let norm = super::validate_source_subdir(sd)?;
            format!("{commit}:{norm}")
        }
        None => commit.to_string(),
    };

    let content_root = staging_base.join("content");
    std::fs::create_dir_all(&content_root)?;

    let output = Command::new("git")
        .args(["--git-dir"])
        .arg(&mirror)
        .args(["archive", "--format=tar", &treeish])
        .output()
        .map_err(|e| SkmError::general(format!("git archive failed to start: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut msg = format!(
            "error: git archive failed for {commit}\n  → {}",
            stderr.trim()
        );
        if subdir.is_some() {
            msg.push_str("\n  → Check that the --subdir subdirectory exists in the repo.");
        }
        return Err(SkmError::general(msg));
    }

    super::tar::extract_tar_bytes(&output.stdout, &content_root)?;
    Ok(content_root)
}
