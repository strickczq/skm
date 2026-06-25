//! Source-artifact cache. Content-addressed, `v1-` schema prefix.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256 as Sha256Hasher};

use crate::sys::hash::Sha256;

use crate::error::{Result, SkmError};
use crate::sys::fsutil::{self, FileLock};

pub struct Cache {
    pub root: PathBuf,
}

/// Minimal URL normalization before key computation: strip trailing `/`
/// and a `.git` suffix. Pure string, no network.
pub fn normalize_url(url: &str) -> String {
    let mut s = url.trim().to_string();
    while s.ends_with('/') {
        s.pop();
    }
    if let Some(stripped) = s.strip_suffix(".git") {
        s = stripped.to_string();
    }
    s
}

pub fn sha256_hex_str(s: &str) -> Sha256 {
    let mut h = Sha256Hasher::new();
    h.update(s.as_bytes());
    Sha256::from_bytes(h.finalize().into())
}

pub fn sha256_hex_bytes(b: &[u8]) -> Sha256 {
    let mut h = Sha256Hasher::new();
    h.update(b);
    Sha256::from_bytes(h.finalize().into())
}

impl Cache {
    /// Open the cache at `root`, creating it if needed, and sweep orphan
    /// staging-write directories.  The caller is responsible for resolving the
    /// cache root (e.g. via [`crate::model::config::cache_dir`]).
    pub fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root).map_err(|e| {
            SkmError::io(format!("cannot create cache dir '{}': {e}", root.display()))
        })?;
        let c = Cache { root };
        c.cleanup_orphan_staging_writes();
        Ok(c)
    }

    pub fn cache_lock_path(&self) -> PathBuf {
        self.root.join(".cache.lock")
    }

    /// Acquire the shared cache lock for the materialization window.
    pub fn lock_shared(&self) -> Result<FileLock> {
        fsutil::acquire_lock(&self.cache_lock_path(), false, true)
    }

    /// Acquire the exclusive cache lock for `cache clean` (fail fast).
    pub fn lock_exclusive_nonblocking(&self) -> Result<FileLock> {
        fsutil::acquire_lock(&self.cache_lock_path(), true, false)
    }

    pub fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// Create a fresh staging directory for materializing a content tree.
    pub fn new_staging(&self, name: &str) -> Result<PathBuf> {
        let dir = self
            .staging_root()
            .join(format!("{name}.{}", fsutil::rand_token()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| SkmError::io(format!("cannot create staging '{}': {e}", dir.display())))?;
        Ok(dir)
    }

    fn staging_write_root(&self) -> PathBuf {
        self.root.join("staging-write")
    }

    /// Create a fresh staging-write dir for atomic cache population.
    pub fn new_staging_write(&self) -> Result<PathBuf> {
        let dir = self.staging_write_root().join(fsutil::rand_token());
        std::fs::create_dir_all(&dir).map_err(|e| {
            SkmError::io(format!(
                "cannot create staging-write '{}': {e}",
                dir.display()
            ))
        })?;
        Ok(dir)
    }

    /// Best-effort removal of orphan staging-write dirs at startup.
    ///
    /// Guarded by a non-blocking `LOCK_EX` on `.cache.lock`: a live writer holds
    /// that lock `LOCK_SH` for the whole materialization window during which its
    /// `staging-write/<rand>` dir exists, so acquiring `LOCK_EX` succeeds only
    /// when no materialization is in flight anywhere (any scope sharing this
    /// global cache). If we cannot acquire it, a sync is materializing — skip the
    /// sweep so we never delete a live writer's in-progress download.
    fn cleanup_orphan_staging_writes(&self) {
        let Ok(_guard) = self.lock_exclusive_nonblocking() else {
            return;
        };
        let root = self.staging_write_root();
        if let Ok(rd) = std::fs::read_dir(&root) {
            for entry in rd.flatten() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Bare git mirror path for a repo URL.
    pub fn git_mirror(&self, repo_url: &str) -> PathBuf {
        let key = format!("v1-git-{}", sha256_hex_str(&normalize_url(repo_url)));
        self.root.join(key)
    }

    /// Directory holding a specific archive version (zip/tar).
    pub fn archive_version_dir(&self, kind: &str, url: &str, archive_sha256: &str) -> PathBuf {
        let key = format!("v1-{kind}-{}", sha256_hex_str(&normalize_url(url)));
        self.root.join(key).join(archive_sha256)
    }

    /// Path of the stored raw archive bytes inside a version dir.
    pub fn archive_file(&self, kind: &str, url: &str, archive_sha256: &str) -> PathBuf {
        self.archive_version_dir(kind, url, archive_sha256)
            .join("archive")
    }

    /// Atomically move a populated staging-write directory to its final key.
    /// If the destination already exists (another writer won), keep theirs.
    pub fn commit(&self, staging_write: &Path, final_dir: &Path) -> Result<()> {
        if final_dir.exists() {
            let _ = std::fs::remove_dir_all(staging_write);
            return Ok(());
        }
        if let Some(parent) = final_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SkmError::io(format!("cannot create '{}': {e}", parent.display())))?;
        }
        match std::fs::rename(staging_write, final_dir) {
            Ok(()) => Ok(()),
            Err(_) if final_dir.exists() => {
                let _ = std::fs::remove_dir_all(staging_write);
                Ok(())
            }
            Err(e) => Err(SkmError::io(format!(
                "cannot commit cache entry '{}': {e}",
                final_dir.display()
            ))),
        }
    }

    /// Remove all cached source artifacts, keeping the staging dir itself.
    pub fn clean(&self) -> Result<()> {
        for entry in std::fs::read_dir(&self.root).map_err(|e| {
            SkmError::io(format!("cannot read cache '{}': {e}", self.root.display()))
        })? {
            let entry = entry.map_err(|e| SkmError::io(format!("read dir entry: {e}")))?;
            let name = entry.file_name().to_string_lossy().to_string();
            // Preserve the runtime lock files: `.cache.lock` (this very lock) and
            // `flocks/` (the per-scope locks). Deleting a live lock file unlinks
            // its inode, so a process re-opening the path gets a *new* inode and
            // its flock no longer mutually-excludes the prior holder — silently
            // breaking scope mutual exclusion.
            if name == ".cache.lock" || name == "flocks" {
                continue;
            }
            if name == "staging" {
                // Only delete orphan subdirs under staging.
                if let Ok(rd) = std::fs::read_dir(entry.path()) {
                    for sub in rd.flatten() {
                        let _ = std::fs::remove_dir_all(sub.path());
                    }
                }
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
        Ok(())
    }
}
