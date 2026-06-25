//! Shared test harness: isolated environment + helpers (no external network).
#![allow(dead_code)]
#![allow(unused_imports)]

mod archive;
mod env;
mod fake_cmd;
mod git_repo;
mod manifest;

pub use archive::*;
pub use env::*;
pub use fake_cmd::*;
pub use git_repo::*;
pub use manifest::*;

use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

pub fn write_file(path: &Path, contents: &str, exec: bool) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, contents).unwrap();
    if exec {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(path, perm).unwrap();
    }
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Run `git` in `dir`, asserting success, and return trimmed stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
