//! Deterministic content hashing.
//!
//! Both call contexts share one core serialization; the only difference is
//! where the executable bit comes from. We unify them: the materializer
//! produces a staging tree whose on-disk modes already reflect the source
//! logical mode (git/tar set `+x` from the source header, zip stays 644, local
//! is copied verbatim). Hashing therefore always reads `st_mode` and applies
//! the per-source policy below — which makes invariant H1 hold by construction.

use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256 as Sha256Hasher};

use crate::error::{Result, SkmError};

/// Whether the executable bit participates in the content hash.
///
/// Owned by `sys` so callers in higher layers convert from their own
/// source-type enums before calling [`hash_tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecPolicy {
    /// Exec bit is included (git, tar sources — modes come from the archive).
    Include,
    /// Exec bit is ignored (zip, local sources — cross-platform consistency).
    Exclude,
}

/// A SHA256 digest (32 bytes). Hex encoding is done at the I/O boundary
/// via [`fmt::Display`] / [`Sha256::to_hex`] / [`Sha256::from_hex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256([u8; 32]);

impl Sha256 {
    /// Build from raw 32 bytes (hash functions).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Sha256(bytes)
    }

    /// Parse from a hex string. Production values are always 64-char, but
    /// the parser accepts any even-length hex for flexibility (tests, etc.).
    pub fn from_hex(hex: &str) -> Result<Self> {
        if hex.len() % 2 != 0 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(SkmError::general(format!(
                "invalid sha256 hex digest: '{hex}'"
            )));
        }
        let n = hex.len() / 2;
        let mut bytes = [0u8; 32];
        if n > 32 {
            return Err(SkmError::general(format!(
                "sha256 hex too long ({hex} chars)"
            )));
        }
        for (i, b) in bytes[..n].iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| SkmError::general("invalid sha256 hex digest"))?;
        }
        Ok(Sha256(bytes))
    }

    /// Render as a 64-char lowercase hex string (allocates).
    pub fn to_hex(self) -> String {
        hex(&self.0)
    }

    /// First 12 chars of the hex digest, for human-facing summaries.
    pub fn short_hex(self) -> String {
        hex(&self.0[..6])
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Compute `content_sha256` over a directory tree.  `exec` controls whether
/// the executable bit participates in the hash (see [`ExecPolicy`]).
/// Used both for the staging tree (materialization context) and for an
/// installed on-disk directory (verification context).
pub fn hash_tree(dir: &Path, exec: ExecPolicy) -> Result<Sha256> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(dir, dir, &mut files)?;

    // Sort ascending by UTF-8 bytes (memcmp order).
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut hasher = Sha256Hasher::new();
    let policy_exec = matches!(exec, ExecPolicy::Include);
    for (rel, abs) in &files {
        let meta = std::fs::symlink_metadata(abs)
            .map_err(|e| SkmError::io(format!("cannot stat '{}': {e}", abs.display())))?;
        // Defensive: a symlink slipping through is an error.
        if meta.file_type().is_symlink() {
            return Err(SkmError::general(format!(
                "symlink not allowed inside a skill: '{}'",
                rel
            )));
        }
        let exec_flag: u8 = if policy_exec && (meta.permissions().mode() & 0o111) != 0 {
            0x01
        } else {
            0x00
        };
        let content = std::fs::read(abs)
            .map_err(|e| SkmError::io(format!("cannot read '{}': {e}", abs.display())))?;

        hasher.update(rel.as_bytes());
        hasher.update([0x00]);
        hasher.update([exec_flag]);
        hasher.update((content.len() as u64).to_le_bytes());
        hasher.update(&content);
    }
    Ok(Sha256::from_bytes(hasher.finalize().into()))
}

/// Recursively collect regular files as `(relative-slash-path, absolute-path)`.
/// Errors on symlinks and other non-regular entries. Empty directories
/// are ignored implicitly (only files are recorded).
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| SkmError::io(format!("cannot read directory '{}': {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| SkmError::io(format!("cannot read dir entry: {e}")))?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| SkmError::io(format!("cannot stat '{}': {e}", path.display())))?;
        if ft.is_symlink() {
            let rel = rel_slash(root, &path)?;
            return Err(SkmError::general(format!(
                "symlink not allowed inside a skill: '{}'",
                rel
            )));
        } else if ft.is_dir() {
            collect_files(root, &path, out)?;
        } else if ft.is_file() {
            let rel = rel_slash(root, &path)?;
            out.push((rel, path));
        } else {
            let rel = rel_slash(root, &path)?;
            return Err(SkmError::general(format!(
                "non-regular file not allowed inside a skill: '{}'",
                rel
            )));
        }
    }
    Ok(())
}

/// Relative path from `root` to `path`, with `/` separators and UTF-8.
fn rel_slash(root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| SkmError::general("path escaped tree root"))?;
    let mut parts = Vec::new();
    for comp in rel.components() {
        let s = comp
            .as_os_str()
            .to_str()
            .ok_or_else(|| SkmError::general("non-UTF-8 path component"))?;
        parts.push(s);
    }
    Ok(parts.join("/"))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, content: &[u8], exec: bool) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        if exec {
            let mut perm = fs::metadata(&p).unwrap().permissions();
            perm.set_mode(0o755);
            fs::set_permissions(&p, perm).unwrap();
        }
    }

    #[test]
    fn deterministic_and_order_independent() {
        let a = tempfile::tempdir().unwrap();
        write(a.path(), "b.txt", b"bbb", false);
        write(a.path(), "a.txt", b"aaa", false);
        let h1 = hash_tree(a.path(), ExecPolicy::Include).unwrap();

        let b = tempfile::tempdir().unwrap();
        write(b.path(), "a.txt", b"aaa", false);
        write(b.path(), "b.txt", b"bbb", false);
        let h2 = hash_tree(b.path(), ExecPolicy::Include).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn exec_bit_changes_git_hash_but_not_zip() {
        let a = tempfile::tempdir().unwrap();
        write(a.path(), "s.sh", b"#!/bin/sh\n", false);
        let b = tempfile::tempdir().unwrap();
        write(b.path(), "s.sh", b"#!/bin/sh\n", true);

        assert_ne!(
            hash_tree(a.path(), ExecPolicy::Include).unwrap(),
            hash_tree(b.path(), ExecPolicy::Include).unwrap()
        );
        // zip ignores exec → identical (cross-platform consistency).
        assert_eq!(
            hash_tree(a.path(), ExecPolicy::Exclude).unwrap(),
            hash_tree(b.path(), ExecPolicy::Exclude).unwrap()
        );
        // local also ignores exec.
        assert_eq!(
            hash_tree(a.path(), ExecPolicy::Exclude).unwrap(),
            hash_tree(b.path(), ExecPolicy::Exclude).unwrap()
        );
    }

    #[test]
    fn known_answer_vector_pins_wire_format() {
        // Cross-implementation contract: the exact serialization is
        // `relpath UTF-8` + `\0` + `exec(1B)` + `len(u64 LE)` + `content`, with
        // NO separators between files, sorted by UTF-8 byte order. These digests
        // were computed independently (Python) from that spec, so a wrong
        // endianness / extra separator / field reorder fails here even though it
        // would stay self-consistent and pass every relative-property test.
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "a.sh", b"x", true); // exec
        write(d.path(), "b.txt", b"yy", false);

        // git: exec bit participates → a.sh contributes exec_flag = 0x01.
        assert_eq!(
            hash_tree(d.path(), ExecPolicy::Include).unwrap().to_hex(),
            "527e8cf6f853f5040ea9dacc972aeb9e5a8599efc01ad8a17fe3b0eccc353540"
        );
        // zip: exec bit dropped → both files contribute exec_flag = 0x00.
        assert_eq!(
            hash_tree(d.path(), ExecPolicy::Exclude).unwrap().to_hex(),
            "507404ed9c65b9757f5286e2cc7b3d601eec1978e402ad4422000dd47af463d8"
        );
    }

    #[test]
    fn symlink_errors() {
        let a = tempfile::tempdir().unwrap();
        write(a.path(), "real.txt", b"x", false);
        std::os::unix::fs::symlink("real.txt", a.path().join("link.txt")).unwrap();
        assert!(hash_tree(a.path(), ExecPolicy::Include).is_err());
    }
}
