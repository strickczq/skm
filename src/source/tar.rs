//! Tar source: download, verify, decompress, and extract preserving Unix modes.
//! Also hosts the shared tar extractor used by the git backend.

use std::io::{Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::error::{ExitCode, Result, SkmError, three_part};
use crate::sys::cache::{self, Cache};
use crate::sys::hash::Sha256;
use crate::ui;

use super::ResolvedSource;

/// Extract an uncompressed tar byte stream into `dest`, preserving file modes
/// and rejecting unsafe / symlink / non-regular entries.
pub fn extract_tar_bytes(bytes: &[u8], dest: &Path) -> Result<()> {
    extract_tar_reader(Cursor::new(bytes), dest)
}

fn extract_tar_reader<R: Read>(reader: R, dest: &Path) -> Result<()> {
    let mut ar = tar::Archive::new(reader);
    let entries = ar
        .entries()
        .map_err(|e| SkmError::general(format!("cannot read tar stream: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| SkmError::general(format!("corrupt tar entry: {e}")))?;
        let etype = entry.header().entry_type();
        // Skip pax/gnu metadata entries (e.g. git archive's pax_global_header).
        if etype.is_pax_global_extensions()
            || etype.is_pax_local_extensions()
            || etype.is_gnu_longname()
            || etype.is_gnu_longlink()
        {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| SkmError::general(format!("bad tar entry path: {e}")))?
            .into_owned();
        let safe = safe_join(dest, &path)?;

        if etype.is_dir() {
            std::fs::create_dir_all(&safe)?;
        } else if etype.is_file() {
            if let Some(parent) = safe.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| SkmError::general(format!("read tar entry: {e}")))?;
            std::fs::write(&safe, &buf)?;
            // Mask dangerous bits (setuid, setgid, sticky, world-writable)
            // before applying to disk. The exec-bit policy is already
            // encoded in the hash via the staging tree; here we only
            // care about filesystem safety.
            let mode = entry.header().mode().unwrap_or(0o644) & 0o755;
            std::fs::set_permissions(&safe, std::fs::Permissions::from_mode(mode))?;
        } else if etype.is_symlink() || etype.is_hard_link() {
            return Err(SkmError::general(format!(
                "symlink not allowed inside a skill: '{}'",
                path.display()
            )));
        } else {
            return Err(SkmError::general(format!(
                "non-regular tar entry not allowed: '{}'",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Join `base` with an archive-relative `path`, rejecting `..` / absolute
/// escapes (zip/tar slip protection).
pub fn safe_join(base: &Path, path: &Path) -> Result<PathBuf> {
    let mut out = base.to_path_buf();
    for comp in path.components() {
        match comp {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(SkmError::general(format!(
                    "unsafe archive entry path escapes extraction root: '{}'",
                    path.display()
                )));
            }
        }
    }
    Ok(out)
}

/// Pick a decompressing reader based on the URL suffix.
pub fn decompress<'a>(url: &str, file: std::fs::File) -> Result<Box<dyn Read + 'a>> {
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Ok(Box::new(flate2::read::GzDecoder::new(file)))
    } else if lower.ends_with(".tar.zst") || lower.ends_with(".tzst") {
        let dec = zstd::Decoder::new(file)
            .map_err(|e| SkmError::general(format!("zstd init failed: {e}")))?;
        Ok(Box::new(dec))
    } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
        Ok(Box::new(xz2::read::XzDecoder::new(file)))
    } else {
        // Plain .tar or unknown suffix → treat as uncompressed.
        Ok(Box::new(file))
    }
}

/// Download archive bytes over HTTP (no auth).
pub fn download(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::blocking::get(url)
        .map_err(|e| SkmError::network(format!("download failed for '{url}': {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(SkmError::network(three_part(
            &format!("error: download failed for '{url}' (HTTP {status})"),
            "the server returned a non-success status",
            "Check the URL. zip/tar sources do not support HTTP authentication; use git or a local source for private artifacts.",
        )));
    }
    let bytes = resp
        .bytes()
        .map_err(|e| SkmError::network(format!("reading body for '{url}': {e}")))?;
    Ok(bytes.to_vec())
}

/// Policy for ensure_archive_cached's sha verification.
pub enum ShaPolicy {
    /// No declared sha — accept whatever the server returns.
    None,
    /// Manifest-declared sha (safety gate, user hex string). Mismatch is a hard error.
    Required(String),
    /// Lock-recorded sha (record only, from lockfile Sha256). Mismatch warns,
    /// new bytes are stored under their actual sha.
    Expected(Sha256),
}

/// Ensure the raw archive is cached, returning the `archive_sha256` of the
/// stored bytes. See [`ShaPolicy`] for verification behavior. Shared by tar and
/// zip via `kind`.
///
/// `frozen_lock_context` causes an offline cache miss to report
/// [`ExitCode::LockMissing`] (6) instead of
/// [`ExitCode::Network`] (4) — per the `--offline` table.
pub fn ensure_archive_cached(
    cache: &Cache,
    kind: &str,
    url: &str,
    policy: ShaPolicy,
    offline: bool,
    frozen_lock_context: bool,
) -> Result<Sha256> {
    // If we know a candidate sha, try cache first.
    let candidate_hex = match &policy {
        ShaPolicy::None => None,
        ShaPolicy::Required(s) => Some(s.clone()),
        ShaPolicy::Expected(rec) => Some(rec.to_hex()),
    };
    if let Some(ref hex) = candidate_hex {
        let file = cache.archive_file(kind, url, hex);
        if file.is_file() {
            return Sha256::from_hex(hex);
        }
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
                &format!("error: archive for '{url}' not in cache and --offline given"),
                "the required artifact is missing locally",
                "Run without --offline to download it.",
            ),
        ));
    }

    let bytes = download(url)?;
    let archive_sha256 = cache::sha256_hex_bytes(&bytes);
    match &policy {
        ShaPolicy::None => {}
        ShaPolicy::Required(decl) if *decl != archive_sha256.to_hex() => {
            return Err(SkmError::general(three_part(
                &format!("error: sha256 mismatch for '{url}'"),
                &format!("declared {decl}, downloaded {archive_sha256}"),
                "Update the sha256 in skm.toml, or verify the source is trusted.",
            )));
        }
        ShaPolicy::Required(_) => {}
        ShaPolicy::Expected(rec) if *rec != archive_sha256 => {
            // Archive bytes drifted but the content gate decides
            // correctness. Warn and store under the new sha.
            ui::warn!(
                "archive_sha256 for '{url}' drifted from the lock (lock {rec}, downloaded {archive_sha256}); content_sha256 will decide."
            );
        }
        ShaPolicy::Expected(_) => {}
    }

    let hex = archive_sha256.to_hex();
    let sw = cache.new_staging_write()?;
    std::fs::write(sw.join("archive"), &bytes)?;
    let final_dir = cache.archive_version_dir(kind, url, &hex);
    cache.commit(&sw, &final_dir)?;
    Ok(archive_sha256)
}

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
    // resolve only runs in the lock phase; --frozen/--locked skip lock entirely,
    // so the lock-context flag is always false here.
    let archive_sha256 = ensure_archive_cached(cache, "tar", url, policy, offline, false)?;
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
    // Ensure present. The lock's archive_sha256 is record-only: if the
    // server now serves repacked bytes, take them and let the content gate
    // decide. The actual stored sha (returned) may differ from the lock's.
    let effective_sha = ensure_archive_cached(
        cache,
        "tar",
        url,
        ShaPolicy::Expected(archive_sha256),
        offline,
        frozen_lock_context,
    )?;
    let hex = effective_sha.to_hex();
    let archive_path = cache.archive_file("tar", url, &hex);
    let file = std::fs::File::open(&archive_path)
        .map_err(|e| SkmError::io(format!("cannot open cached archive: {e}")))?;
    let reader = decompress(url, file)?;

    let extract_root = staging_base.join("extract");
    std::fs::create_dir_all(&extract_root)?;
    extract_tar_reader(reader, &extract_root)?;
    super::content_root_with_subdir(&extract_root, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-rolled ustar header helper: creates a regular file entry with
    /// the given name, body, and raw mode bits.
    fn ustar_entry(name: &str, body: &[u8], mode: u32) -> Vec<u8> {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        h[..nb.len()].copy_from_slice(nb);
        // mode at bytes 100–108 (octal, space-padded)
        let mode_str = format!("{mode:07o} \0");
        h[100..100 + mode_str.len()].copy_from_slice(mode_str.as_bytes());
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        for b in &mut h[148..156] {
            *b = b' ';
        }
        h[156] = b'0'; // regular file
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        // checksum: sum of all bytes treating checksum field as spaces
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(body);
        let pad = (512 - body.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
        out
    }

    fn finish_tar(entries: Vec<u8>) -> Vec<u8> {
        let mut out = entries;
        out.extend(std::iter::repeat_n(0u8, 1024)); // two zero blocks
        out
    }

    #[test]
    fn dangerous_mode_bits_are_masked() {
        let tar = finish_tar(ustar_entry("evil.sh", b"#!/bin/sh\n", 0o4777));
        let tmp = tempfile::tempdir().unwrap();
        extract_tar_bytes(&tar, tmp.path()).unwrap();
        let mode = std::fs::metadata(tmp.path().join("evil.sh"))
            .unwrap()
            .permissions()
            .mode();
        // setuid + world-writable are stripped; exec bit preserved.
        assert_eq!(mode & 0o7777, 0o755, "dangerous bits must be masked");
    }

    #[test]
    fn normal_mode_passes_unchanged() {
        let tar = finish_tar(ustar_entry("s.sh", b"echo ok\n", 0o755));
        let tmp = tempfile::tempdir().unwrap();
        extract_tar_bytes(&tar, tmp.path()).unwrap();
        let mode = std::fs::metadata(tmp.path().join("s.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o755, "normal exec mode preserved");
    }
}
