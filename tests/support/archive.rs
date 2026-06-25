//! Archive builder + SHA-256 hashing.

use std::io::Write;

// ---------------------------------------------------------------------------
// Archive builder
// ---------------------------------------------------------------------------

/// Builder for in-memory zip / tar.gz archives.
///
/// ```rust
/// let zip = Archive::zip()
///     .file("SKILL.md", "# z\n", false)
///     .build();
/// let tar = Archive::tar_gz()
///     .file("SKILL.md", "# t\n", false)
///     .file("run.sh", "#!/bin/sh\n", true)
///     .build();
/// ```
pub struct ArchiveBuilder {
    entries: Vec<(String, String, bool)>,
    format: ArchiveFormat,
}

enum ArchiveFormat {
    Zip,
    TarGz(flate2::Compression),
}

/// Namespace for archive constructors.
pub struct Archive;

impl Archive {
    pub fn zip() -> ArchiveBuilder {
        ArchiveBuilder {
            entries: Vec::new(),
            format: ArchiveFormat::Zip,
        }
    }

    pub fn tar_gz() -> ArchiveBuilder {
        ArchiveBuilder {
            entries: Vec::new(),
            format: ArchiveFormat::TarGz(flate2::Compression::default()),
        }
    }

    /// Same as [`tar_gz`] but with a caller-chosen gzip compression level.
    pub fn tar_gz_with(level: flate2::Compression) -> ArchiveBuilder {
        ArchiveBuilder {
            entries: Vec::new(),
            format: ArchiveFormat::TarGz(level),
        }
    }
}

impl ArchiveBuilder {
    /// Add a file entry. Returns Self for chaining.
    pub fn file(mut self, name: &str, content: &str, exec: bool) -> Self {
        self.entries
            .push((name.to_string(), content.to_string(), exec));
        self
    }

    /// Build the archive in memory and return the raw bytes.
    pub fn build(self) -> Vec<u8> {
        match self.format {
            ArchiveFormat::Zip => build_zip_impl(&self.entries),
            ArchiveFormat::TarGz(level) => build_targz_impl(&self.entries, level),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal implementations
// ---------------------------------------------------------------------------

fn build_zip_impl(entries: &[(String, String, bool)]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        for (name, contents, exec) in entries {
            let mode = if *exec { 0o755 } else { 0o644 };
            let opts = SimpleFileOptions::default().unix_permissions(mode);
            w.start_file(name, opts).unwrap();
            w.write_all(contents.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

fn build_targz_impl(entries: &[(String, String, bool)], level: flate2::Compression) -> Vec<u8> {
    use flate2::write::GzEncoder;
    let mut out = Vec::new();
    {
        let enc = GzEncoder::new(&mut out, level);
        let mut tar = tar::Builder::new(enc);
        for (name, contents, exec) in entries {
            let bytes = contents.as_bytes();
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(if *exec { 0o755 } else { 0o644 });
            header.set_cksum();
            tar.append_data(&mut header, name, bytes).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }
    out
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
