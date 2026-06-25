//! Env – isolated skm environment with assertion helpers.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{read, write_file};

/// An isolated skm environment with its own HOME / config / cache / project.
pub struct Env {
    pub _tmp: TempDir,
    pub home: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
    pub project: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        let home = base.join("home");
        let config = base.join("config");
        let cache = base.join("cache");
        let project = base.join("project");
        for d in [&home, &config, &cache, &project] {
            std::fs::create_dir_all(d).unwrap();
        }
        Env {
            _tmp: tmp,
            home,
            config,
            cache,
            project,
        }
    }

    /// A skm command rooted at the project directory.
    pub fn skm(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("skm").unwrap();
        cmd.current_dir(&self.project)
            .env("HOME", &self.home)
            .env("SKM_CONFIG_DIR", &self.config)
            .env("SKM_CACHE_DIR", &self.cache)
            // Make prune non-interactive deterministically.
            .env_remove("CI");
        cmd
    }

    /// A skm command rooted at an arbitrary directory.
    pub fn skm_in(&self, dir: &Path) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("skm").unwrap();
        cmd.current_dir(dir)
            .env("HOME", &self.home)
            .env("SKM_CONFIG_DIR", &self.config)
            .env("SKM_CACHE_DIR", &self.cache);
        cmd
    }

    pub fn agents_root(&self) -> PathBuf {
        self.project.join(".agents").join("skills")
    }

    pub fn claude_root(&self) -> PathBuf {
        self.project.join(".claude").join("skills")
    }

    pub fn codex_root(&self) -> PathBuf {
        self.project.join(".codex").join("skills")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.project.join("skm.lock")
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.project.join("skm.toml")
    }

    // ------------------------------------------------------------------
    // Source setup
    // ------------------------------------------------------------------

    /// Create a local skill directory under `vendor/<name>` with the given
    /// files and return its path. SKILL.md is created automatically if not
    /// in `files`.
    pub fn create_local_skill(&self, name: &str, files: &[(&str, &str, bool)]) -> PathBuf {
        let dir = self.project.join(format!("vendor/{name}"));
        let has_skill_md = files.iter().any(|(p, _, _)| *p == "SKILL.md");
        if !has_skill_md {
            write_file(&dir.join("SKILL.md"), &format!("# {name}\n"), false);
        }
        for (rel, contents, exec) in files {
            write_file(&dir.join(rel), contents, *exec);
        }
        dir
    }

    // ------------------------------------------------------------------
    // Assertions
    // ------------------------------------------------------------------

    /// `skm status` reports clean (exit 0).
    pub fn assert_clean(&self) {
        self.skm().arg("status").assert().success();
    }

    /// `skm status` reports drift (exit 2).
    pub fn assert_drift(&self) {
        self.skm().arg("status").assert().code(2);
    }

    /// The skill directory exists under the agents root and contains SKILL.md.
    pub fn assert_installed(&self, name: &str) {
        let p = self.agents_root().join(name).join("SKILL.md");
        assert!(
            p.is_file(),
            "skill '{name}' not installed at {}",
            p.display()
        );
    }

    /// The skill directory does NOT exist under the agents root.
    pub fn assert_not_installed(&self, name: &str) {
        let p = self.agents_root().join(name);
        assert!(
            !p.exists(),
            "skill '{name}' unexpectedly present at {}",
            p.display()
        );
    }

    /// Extract `content_sha256` for a skill name from the lock file.
    pub fn lock_sha256(&self, name: &str) -> String {
        let lock = read(&self.lock_path());
        let mut found_name = false;
        for line in lock.lines() {
            if line.contains(&format!("name = \"{name}\"")) {
                found_name = true;
            }
            if found_name && line.contains("content_sha256") {
                return line
                    .split('"')
                    .nth(1)
                    .unwrap_or_else(|| panic!("malformed content_sha256 line in lock: {line}"))
                    .to_string();
            }
        }
        panic!("content_sha256 not found for skill '{name}' in lock file:\n{lock}");
    }

    /// Read the raw lock file text.
    pub fn lock_text(&self) -> String {
        read(&self.lock_path())
    }

    /// Read the raw manifest text.
    pub fn manifest_text(&self) -> String {
        read(&self.manifest_path())
    }
}
