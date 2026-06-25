//! Archive extraction safety (zip slip / tar slip): a malicious archive
//! whose entry path escapes the staging root via `..` must be rejected, and
//! nothing may be written outside the extraction root.

mod support;
use support::*;

use std::io::Write;

/// A zip whose second entry escapes via `../` (the `zip` crate's `start_file`
/// does not sanitize names).
fn evil_zip() -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let o = SimpleFileOptions::default();
        w.start_file("SKILL.md", o).unwrap();
        w.write_all(b"# z\n").unwrap();
        w.start_file("../escape.txt", o).unwrap();
        w.write_all(b"pwned\n").unwrap();
        w.finish().unwrap();
    }
    buf.into_inner()
}

/// Hand-rolled POSIX ustar — the `tar` crate rejects `..` in `set_path`, so we
/// emit the raw 512-byte header to smuggle an escaping entry path.
fn ustar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        h[..nb.len()].copy_from_slice(nb);
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        for b in &mut h[148..156] {
            *b = b' ';
        }
        h[156] = b'0'; // regular file
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        let pad = (512 - data.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    out.extend(std::iter::repeat_n(0u8, 1024)); // two zero blocks
    out
}

fn gz(data: &[u8]) -> Vec<u8> {
    use flate2::write::GzEncoder;
    let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

#[test]
fn zip_slip_entry_is_rejected() {
    let env = Env::new();
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/evil.zip")
        .with_body(evil_zip())
        .create();
    let url = format!("{}/evil.zip", server.url());
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::zip("z", &url))
        .write_to(&env.manifest_path());

    let out = env.skm().arg("sync").output().unwrap();
    assert!(!out.status.success(), "zip slip must abort");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("escape.txt"),
        "expected entry name in error; got {err}"
    );
    env.assert_not_installed("z");
    assert!(!env.project.join("escape.txt").exists(), "no write escaped");
}

#[test]
fn tar_slip_entry_is_rejected() {
    let env = Env::new();
    let tar = gz(&ustar(&[
        ("SKILL.md", b"# t\n"),
        ("../escape.txt", b"pwned\n"),
    ]));
    let mut server = mockito::Server::new();
    server.mock("GET", "/evil.tar.gz").with_body(tar).create();
    let url = format!("{}/evil.tar.gz", server.url());
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::tar("t", &url))
        .write_to(&env.manifest_path());

    let out = env.skm().arg("sync").output().unwrap();
    assert!(!out.status.success(), "tar slip must abort");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("escape.txt"),
        "expected entry name in error; got {err}"
    );
    env.assert_not_installed("t");
    assert!(!env.project.join("escape.txt").exists(), "no write escaped");
}
