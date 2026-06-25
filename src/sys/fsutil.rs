//! Filesystem utilities: BSD flock, atomic rename, recursive copy (reflink
//! preferred), temp directories, and crash-recovery GC.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, flock};

use crate::error::{Result, SkmError};
use crate::ui;

/// An held advisory file lock. Released on drop (the underlying fd closes).
pub struct FileLock {
    _file: File,
}

/// Acquire a lock on `path`, creating the lock file if needed.
///
/// * `exclusive` → `LOCK_EX`, otherwise `LOCK_SH`.
/// * `blocking` → wait, else fail fast with [`crate::error::ExitCode::Io`] for
///   the scope lock and a caller-chosen message.
pub fn acquire_lock(path: &Path, exclusive: bool, blocking: bool) -> Result<FileLock> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            SkmError::io(format!(
                "cannot create lock dir '{}': {e}",
                parent.display()
            ))
        })?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| SkmError::io(format!("cannot open lock file '{}': {e}", path.display())))?;

    let op = match (exclusive, blocking) {
        (true, true) => FlockOperation::LockExclusive,
        (true, false) => FlockOperation::NonBlockingLockExclusive,
        (false, true) => FlockOperation::LockShared,
        (false, false) => FlockOperation::NonBlockingLockShared,
    };
    flock(&file, op).map_err(|e| {
        SkmError::io(format!(
            "could not acquire lock on '{}': {e}",
            path.display()
        ))
    })?;
    Ok(FileLock { _file: file })
}

/// Recursively copy `src` directory into `dst`, preferring reflink/CoW and
/// falling back to a byte copy. File modes are preserved.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .map_err(|e| SkmError::io(format!("cannot create '{}': {e}", dst.display())))?;
    for entry in std::fs::read_dir(src)
        .map_err(|e| SkmError::io(format!("cannot read '{}': {e}", src.display())))?
    {
        let entry = entry.map_err(|e| SkmError::io(format!("read dir entry: {e}")))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry
            .file_type()
            .map_err(|e| SkmError::io(format!("stat '{}': {e}", from.display())))?;
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_file() {
            // reflink if possible, else full copy.
            if reflink_copy::reflink_or_copy(&from, &to).is_err() {
                std::fs::copy(&from, &to)
                    .map_err(|e| SkmError::io(format!("copy '{}': {e}", from.display())))?;
            }
            // Preserve mode explicitly (reflink_or_copy does, plain copy does too,
            // but be defensive across fallbacks).
            let mode = std::fs::metadata(&from)
                .map_err(|e| SkmError::io(format!("stat '{}': {e}", from.display())))?
                .permissions()
                .mode();
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode))
                .map_err(|e| SkmError::io(format!("chmod '{}': {e}", to.display())))?;
        } else {
            return Err(SkmError::general(format!(
                "refusing to copy non-regular entry '{}'",
                from.display()
            )));
        }
    }
    Ok(())
}

/// Generate a short random token for temp directory suffixes.
pub fn rand_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{pid:x}{nanos:x}")
}

/// Atomic directory rename (same volume).
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).map_err(|e| {
        SkmError::io(format!(
            "rename '{}' -> '{}': {e}",
            from.display(),
            to.display()
        ))
    })
}

/// Recursively remove a directory, ignoring "not found".
pub fn remove_dir_all(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SkmError::io(format!(
            "cannot remove '{}': {e}",
            path.display()
        ))),
    }
}

/// Atomically remove a managed directory: rename to an `old`-temp name then
/// delete, falling back to a direct `remove_dir_all` if the rename fails.
/// The rename makes the removal atomic from the perspective of concurrent
/// readers of the skills root.  Shared by prune and `skm remove`.
pub fn atomic_remove_dir(root: &Path, name: &str) -> Result<()> {
    let path = root.join(name);
    let old_at = old_path(root, name);
    if rename(&path, &old_at).is_ok() {
        remove_dir_all(&old_at)?;
    } else {
        remove_dir_all(&path)?;
    }
    Ok(())
}

const NEW_MARK: &str = ".skm-new.";
const OLD_MARK: &str = ".skm-old.";

/// Build the transient new/old paths next to `<root>/<name>`.
pub fn new_path(root: &Path, name: &str) -> PathBuf {
    root.join(format!("{name}{NEW_MARK}{}", rand_token()))
}
pub fn old_path(root: &Path, name: &str) -> PathBuf {
    root.join(format!("{name}{OLD_MARK}{}", rand_token()))
}

/// Returns true if a directory entry name is a transient skm temp dir.
pub fn is_temp_name(name: &str) -> bool {
    name.contains(NEW_MARK) || name.contains(OLD_MARK)
}

/// Extract the base skill name from a temp entry name, plus which kind it is.
fn parse_temp(name: &str) -> Option<(String, bool)> {
    if let Some(idx) = name.find(NEW_MARK) {
        Some((name[..idx].to_string(), true))
    } else {
        name.find(OLD_MARK)
            .map(|idx| (name[..idx].to_string(), false))
    }
}

/// Crash-recovery GC over a skills root. Rollback (rule 3) runs before
/// deletions (rules 1/2). Only call when holding `LOCK_EX` (write commands).
pub fn gc_skills_root(root: &Path) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut news: Vec<(String, PathBuf)> = Vec::new();
    let mut olds: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(root)
        .map_err(|e| SkmError::io(format!("cannot read '{}': {e}", root.display())))?
    {
        let entry = entry.map_err(|e| SkmError::io(format!("read dir entry: {e}")))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some((base, is_new)) = parse_temp(&name) {
            if is_new {
                news.push((base, entry.path()));
            } else {
                olds.push((base, entry.path()));
            }
        }
    }

    // Rule 3 first: paired new+old means a crash between the two renames.
    let mut handled_news: Vec<usize> = Vec::new();
    let mut handled_olds: Vec<usize> = Vec::new();
    for (oi, (obase, opath)) in olds.iter().enumerate() {
        if let Some((ni, (_, npath))) = news
            .iter()
            .enumerate()
            .find(|(_, (nbase, _))| nbase == obase)
        {
            let final_path = root.join(obase);
            if final_path.exists() {
                // Rule 4 extreme: final recreated externally; keep old, drop new, warn.
                let _ = remove_dir_all(npath);
                ui::warn!(
                    "cannot auto-rollback '{}': '{}' was recreated externally.\n  → kept the previous version at '{}'\n  → run 'rm -rf {} && mv {} {}' to restore the previous version",
                    obase,
                    final_path.display(),
                    opath.display(),
                    final_path.display(),
                    opath.display(),
                    final_path.display(),
                );
            } else {
                // Roll back: old → final, drop new.
                rename(opath, &final_path)?;
                let _ = remove_dir_all(npath);
            }
            handled_news.push(ni);
            handled_olds.push(oi);
        }
    }

    // Rules 1/2: delete unpaired temp dirs.
    for (i, (_, p)) in news.iter().enumerate() {
        if !handled_news.contains(&i) {
            remove_dir_all(p)?;
        }
    }
    for (i, (_, p)) in olds.iter().enumerate() {
        if !handled_olds.contains(&i) {
            remove_dir_all(p)?;
        }
    }
    Ok(())
}

/// Count leftover temp dirs (for `doctor`).
pub fn count_temp_dirs(root: &Path) -> usize {
    if !root.is_dir() {
        return 0;
    }
    std::fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| is_temp_name(&e.file_name().to_string_lossy()))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create `<root>/<name>` as a directory containing a marker file.
    fn mkdir_with(root: &Path, name: &str, marker: &str) -> PathBuf {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), marker).unwrap();
        d
    }

    #[test]
    fn unpaired_temps_are_deleted() {
        // Orphan *.skm-new.* / *.skm-old.* with no partner gone.
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        mkdir_with(root, "foo.skm-new.aaa", "new");
        mkdir_with(root, "bar.skm-old.bbb", "old");
        mkdir_with(root, "keep", "real");

        gc_skills_root(root).unwrap();

        assert!(!root.join("foo.skm-new.aaa").exists());
        assert!(!root.join("bar.skm-old.bbb").exists());
        assert!(root.join("keep").exists(), "real skill untouched");
    }

    #[test]
    fn paired_rolls_back_old_to_final() {
        // Rule 3 (must precede 1/2): a crash between rename(final→old) and
        // rename(new→final) is recovered by restoring old, NOT by deleting it.
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        mkdir_with(root, "x.skm-old.999", "PREVIOUS");
        mkdir_with(root, "x.skm-new.888", "HALF");

        gc_skills_root(root).unwrap();

        // old restored as the live dir; new dropped; no temps remain.
        assert_eq!(
            std::fs::read_to_string(root.join("x/SKILL.md")).unwrap(),
            "PREVIOUS",
            "old version must be rolled back into place (data-loss guard)"
        );
        assert!(!root.join("x.skm-new.888").exists());
        assert!(!root.join("x.skm-old.999").exists());
    }

    #[test]
    fn paired_with_existing_final_keeps_old_and_warns() {
        // Rule 4 extreme: final was recreated externally → keep old_at,
        // drop new_at, leave final untouched, and do NOT error.
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        mkdir_with(root, "x", "RECREATED");
        mkdir_with(root, "x.skm-old.999", "PREVIOUS");
        mkdir_with(root, "x.skm-new.888", "HALF");

        gc_skills_root(root).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("x/SKILL.md")).unwrap(),
            "RECREATED",
            "externally recreated final must be preserved"
        );
        // old kept for manual recovery; new dropped.
        let old_kept = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(OLD_MARK));
        assert!(old_kept, "old_at kept for manual recovery");
        assert!(!root.join("x.skm-new.888").exists());
    }

    #[test]
    fn missing_root_is_noop() {
        let t = tempfile::tempdir().unwrap();
        gc_skills_root(&t.path().join("nope")).unwrap();
    }
}
