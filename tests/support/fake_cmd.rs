//! FakeCmd — intercept a shell command for error-path testing.

use std::path::{Path, PathBuf};

use super::write_file;

/// A fake binary that intercepts calls to a command, matching on `$1`.
///
/// Writes a shell script named `<cmd>` into a temp directory under `home` that
/// either delegates to the real binary or returns custom stderr + exit code.
///
/// ```rust
/// let fake = FakeCmd::new("git", &which_git())
///     .on_arg("ls-remote", 128, "fatal: auth error")
///     .install(&env.home);
/// env.skm().env("PATH", &fake.path()).args([...]).assert().failure();
/// ```
pub struct FakeCmd {
    dir: PathBuf,
    cmd: String,
    real: String,
    cases: Vec<(String, i32, String)>,
}

impl FakeCmd {
    /// Create a new fake for `cmd` that delegates unmatched calls to `real_path`.
    pub fn new(cmd: &str, real_path: &str) -> Self {
        FakeCmd {
            dir: PathBuf::new(), // set in install()
            cmd: cmd.to_string(),
            real: real_path.to_string(),
            cases: Vec::new(),
        }
    }

    /// When `$1` matches `matcher`, exit with `status` and write `stderr` to
    /// stderr (no newline appended — include one yourself if needed).
    pub fn on_arg(mut self, matcher: &str, status: i32, stderr: &str) -> Self {
        self.cases
            .push((matcher.to_string(), status, stderr.to_string()));
        self
    }

    /// Install the script: write it to a temp directory under `home` and make
    /// it executable.
    pub fn install(mut self, home: &Path) -> Self {
        let dir = home.join("fake-bin");
        std::fs::create_dir_all(&dir).unwrap();

        let mut script = String::from("#!/bin/sh\n");
        for (matcher, status, stderr) in &self.cases {
            script.push_str(&format!(
                "case \"$1\" in\n  {matcher})\n    echo '{stderr}' >&2\n    exit {status}\n    ;;\nesac\n"
            ));
        }
        let real = &self.real;
        script.push_str(&format!("exec {real} \"$@\"\n"));

        let path = dir.join(&self.cmd);
        write_file(&path, &script, true);
        self.dir = dir;
        self
    }

    /// Return `self.dir:PATH` for injection into the child process environment.
    pub fn path(&self) -> String {
        format!(
            "{}:{}",
            self.dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }
}
