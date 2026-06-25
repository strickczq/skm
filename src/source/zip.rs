//! Zip source: download, verify, extract. The executable bit is intentionally
//! ignored — everything lands as 0644.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::{Result, SkmError};
use crate::sys::cache::Cache;
use crate::sys::hash::Sha256;

use super::ResolvedSource;
use super::tar::{ShaPolicy, ensure_archive_cached, safe_join};

pub fn resolve(
    cache: &Cache,
    url: &str,
    declared_sha256: Option<&str>,
    offline: bool,
) -> Result<ResolvedSource> {
    let _guard = cache.lock_shared()?;
    let policy = match declared_sha256 {
        Some(s) => ShaPolicy::Required(s.to_string()),
        None => ShaPolicy::None,
    };
    // resolve only runs in the lock phase; --frozen/--locked skip it.
    let archive_sha256 = ensure_archive_cached(cache, "zip", url, policy, offline, false)?;
    Ok(ResolvedSource::Archive {
        sha256: archive_sha256,
    })
}

pub fn materialize(
    cache: &Cache,
    url: &str,
    archive_sha256: Sha256,
    path: Option<&str>,
    offline: bool,
    frozen_lock_context: bool,
    staging_base: &Path,
) -> Result<PathBuf> {
    let _guard = cache.lock_shared()?;
    let effective_sha = ensure_archive_cached(
        cache,
        "zip",
        url,
        ShaPolicy::Expected(archive_sha256),
        offline,
        frozen_lock_context,
    )?;
    let hex = effective_sha.to_hex();
    let archive_path = cache.archive_file("zip", url, &hex);
    let file = std::fs::File::open(&archive_path)
        .map_err(|e| SkmError::io(format!("cannot open cached archive: {e}")))?;

    let extract_root = staging_base.join("extract");
    std::fs::create_dir_all(&extract_root)?;
    extract_zip(file, &extract_root)?;
    super::content_root_with_subdir(&extract_root, path)
}

fn extract_zip(file: std::fs::File, dest: &Path) -> Result<()> {
    let mut archive = ::zip::ZipArchive::new(file)
        .map_err(|e| SkmError::general(format!("cannot read zip: {e}")))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| SkmError::general(format!("corrupt zip entry: {e}")))?;
        let rel = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => {
                return Err(SkmError::general(format!(
                    "unsafe zip entry path: '{}'",
                    entry.name()
                )));
            }
        };
        let out = safe_join(dest, &rel)?;

        // Detect symlinks (stored via unix mode S_IFLNK) and reject.
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                return Err(SkmError::general(format!(
                    "symlink not allowed inside a skill: '{}'",
                    rel.display()
                )));
            }
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| SkmError::general(format!("read zip entry: {e}")))?;
        std::fs::write(&out, &buf)?;
        // Zip lands uniformly as 0644: exec bit not honored.
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}
