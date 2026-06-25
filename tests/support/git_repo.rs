//! GitRepo — temporary git repo with state.

use std::path::{Path, PathBuf};

use super::{Env, git, write_file};

/// A temporary git repo that knows its path, URL, and HEAD.
///
/// ```rust
/// let repo = GitRepo::init(&env, "repo")
///     .file("SKILL.md", "# s\n", false)
///     .file("run.sh", "#!/bin/sh\n", true)
///     .commit("init");
/// let url = repo.url();          // file://...
/// let head = repo.head();        // 40-char commit SHA
/// repo.commit_file("SKILL.md", "# v2\n", "v2");  // modify + commit
/// ```
pub struct GitRepo {
    dir: PathBuf,
}

impl GitRepo {
    /// Create and init an empty git repo under the env's temp dir.
    pub fn init(env: &Env, name: &str) -> Self {
        let dir = env._tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "tester"]);
        GitRepo { dir }
    }

    /// Write a file into the working tree. Does not stage or commit.
    /// Returns Self for chaining.
    pub fn file(self, rel: &str, content: &str, exec: bool) -> Self {
        write_file(&self.dir.join(rel), content, exec);
        self
    }

    /// Stage all files and commit with the given message. Returns Self.
    pub fn commit(self, msg: &str) -> Self {
        git(&self.dir, &["add", "-A"]);
        git(&self.dir, &["commit", "-qm", msg]);
        self
    }

    /// `file://` URL for the repo.
    pub fn url(&self) -> String {
        format!("file://{}", self.dir.display())
    }

    /// Path to the repo working directory.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// HEAD commit SHA (40 hex chars).
    pub fn head(&self) -> String {
        git(&self.dir, &["rev-parse", "HEAD"])
    }

    /// Write a file into the working tree, stage all, and commit.
    /// Convenience for modifying an already-committed repo.
    pub fn commit_file(&self, rel: &str, content: &str, msg: &str) {
        write_file(&self.dir.join(rel), content, false);
        git(&self.dir, &["add", "-A"]);
        git(&self.dir, &["commit", "-qm", msg]);
    }
}
