//! Local source: a mutable path dependency, re-read every sync. Not
//! cached. Path is lexically normalized (no symlink resolution).

use std::path::{Component, Path, PathBuf};

use crate::error::{Result, SkmError, three_part};
use crate::sys::fsutil;

/// Lexically normalize a locator relative to `manifest_dir`. Does not
/// resolve symlinks. For a relative locator the candidate path is joined onto
/// `manifest_dir` *then* collapsed, so a no-net-escape locator like
/// `../proj/vendor/x` from `/x/proj` unifies with `./vendor/x`. Errors if the
/// collapsed result escapes `manifest_dir`.
pub fn normalize_local(manifest_dir: &Path, locator: &str) -> Result<PathBuf> {
    let lp = Path::new(locator);
    let combined = if lp.is_absolute() {
        lp.to_path_buf()
    } else {
        manifest_dir.join(lp)
    };
    let abs = lexical_collapse(&combined);

    if !lp.is_absolute() {
        let base = lexical_collapse(manifest_dir);
        if !abs.starts_with(&base) {
            return Err(SkmError::general(three_part(
                &format!("error: local source '{locator}' escapes the manifest directory"),
                "the path normalizes above the directory containing skm.toml",
                "Use a path inside the project (a symlink may point elsewhere), or a git/tar source.",
            )));
        }
    }
    Ok(abs)
}

/// Lexically collapse `.` and `..` in a path without touching the filesystem.
fn lexical_collapse(path: &Path) -> PathBuf {
    let mut stack: Vec<std::ffi::OsString> = Vec::new();
    let mut has_root = false;
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => has_root = true,
            Component::CurDir => {}
            Component::Normal(s) => stack.push(s.to_os_string()),
            Component::ParentDir => {
                stack.pop();
            }
        }
    }
    let mut out = if has_root {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for c in &stack {
        out.push(c);
    }
    out
}

/// Validate a local source: must exist and be a directory. Errors
/// distinguish a dangling-symlink path resolution failure.
pub fn validate(manifest_dir: &Path, locator: &str) -> Result<PathBuf> {
    let abs = normalize_local(manifest_dir, locator)?;
    match std::fs::metadata(&abs) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(SkmError::general(three_part(
                    &format!("error: local source '{locator}' is not a directory"),
                    "a skill must be a directory containing SKILL.md",
                    "Point the local source at a directory.",
                )));
            }
            Ok(abs)
        }
        Err(e) => {
            // Distinguish dangling symlink (path resolution) from plain IO error.
            if path_has_dangling_symlink(&abs) {
                Err(SkmError::io(format!(
                    "error: local source path resolution failed (symlink target missing?): '{}'",
                    abs.display()
                )))
            } else {
                Err(SkmError::io(format!(
                    "error: cannot read local source directory: '{}' ({e})",
                    abs.display()
                )))
            }
        }
    }
}

/// Heuristic: walk ancestors; if some ancestor is a symlink whose target is
/// missing, treat as a dangling-symlink resolution failure.
fn path_has_dangling_symlink(path: &Path) -> bool {
    let mut cur = PathBuf::new();
    for comp in path.components() {
        cur.push(comp.as_os_str());
        if let Ok(meta) = std::fs::symlink_metadata(&cur) {
            if meta.file_type().is_symlink() && std::fs::metadata(&cur).is_err() {
                return true;
            }
        }
    }
    false
}

/// Materialize by copying the source tree into `staging_base/content`,
/// preserving modes verbatim (local landing mode).
pub fn materialize(manifest_dir: &Path, locator: &str, staging_base: &Path) -> Result<PathBuf> {
    let abs = validate(manifest_dir, locator)?;
    let content_root = staging_base.join("content");
    fsutil::copy_tree(&abs, &content_root)?;
    Ok(content_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_dot_and_collapses() {
        let base = Path::new("/home/u/proj");
        let a = normalize_local(base, "./vendor/reviewer").unwrap();
        assert_eq!(a, PathBuf::from("/home/u/proj/vendor/reviewer"));
        let b = normalize_local(base, "vendor/reviewer").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn escape_errors() {
        let base = Path::new("/home/u/proj");
        assert!(normalize_local(base, "../../etc").is_err());
    }

    #[test]
    fn parent_re_enter_unifies_with_dot_path() {
        // Example: `../proj/vendor/x` from /x/proj == `./vendor/x`.
        let base = Path::new("/x/proj");
        let a = normalize_local(base, "./vendor/x").unwrap();
        let b = normalize_local(base, "../proj/vendor/x").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, PathBuf::from("/x/proj/vendor/x"));
    }

    #[test]
    fn parent_then_back_does_not_escape() {
        let base = Path::new("/home/u/proj");
        // Net stays inside manifest_dir → ok.
        let p = normalize_local(base, "../proj/sub/deep").unwrap();
        assert_eq!(p, PathBuf::from("/home/u/proj/sub/deep"));
    }
}
