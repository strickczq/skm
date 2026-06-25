//! Error types and process exit codes.
//!
//! The CLI maps every failure to one of the documented exit codes so that CI
//! pipelines can react deterministically.

use std::fmt;

/// Documented exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// 0 = success, no change.
    Success = 0,
    /// 1 = general error (bad argument, parse failure, name not found, lock
    /// missing for non-frozen/locked status).
    General = 1,
    /// 2 = drift reported by a read-only command (`status` / `doctor`).
    Drift = 2,
    /// 3 = lock / content-mismatch abort: a `--locked`/`--frozen` gate
    /// failure (determination failed, or materialized content ≠ lock), **or** a
    /// normal-sync foreign-guard abort refusing to overwrite an external
    /// directory whose content differs. "The guard refused passage",
    /// distinct from code 2 ("drift observed" by a read-only command).
    MismatchAbort = 3,
    /// 4 = network error (fetch/download/auth failure, `--offline` cache miss).
    Network = 4,
    /// 5 = permission / IO error (incl. flock acquisition failure).
    Io = 5,
    /// 6 = lock missing under `--frozen` / `--locked`.
    LockMissing = 6,
    /// 10 = `--dry-run` detected changes pending execution.
    DryRunChanges = 10,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        self as i32
    }

    /// Severity rank for choosing the worst exit code across a set of results.
    /// Higher = more severe.  Used by both `skm status` and `skm doctor` so the
    /// ordering lives in a single place.
    pub fn severity_rank(self) -> u32 {
        match self {
            ExitCode::Success => 0,
            ExitCode::DryRunChanges => 0,
            ExitCode::General => 1,
            ExitCode::MismatchAbort => 2,
            ExitCode::Drift => 2,
            ExitCode::Network => 3,
            ExitCode::LockMissing => 3,
            ExitCode::Io => 4,
        }
    }
}

/// The application error. Carries the human-facing message plus the exit code
/// it should map to. Messages should follow the three-part style where
/// appropriate (built via [`three_part`]).
#[derive(Debug)]
pub struct SkmError {
    pub message: String,
    pub exit: ExitCode,
}

impl SkmError {
    pub fn new(exit: ExitCode, message: impl Into<String>) -> Self {
        SkmError {
            message: message.into(),
            exit,
        }
    }

    pub fn general(message: impl Into<String>) -> Self {
        SkmError::new(ExitCode::General, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        SkmError::new(ExitCode::Io, message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        SkmError::new(ExitCode::Network, message)
    }

    pub fn mismatch_abort(message: impl Into<String>) -> Self {
        SkmError::new(ExitCode::MismatchAbort, message)
    }

    pub fn lock_missing(message: impl Into<String>) -> Self {
        SkmError::new(ExitCode::LockMissing, message)
    }
}

impl fmt::Display for SkmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SkmError {}

/// Convenience result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, SkmError>;

/// Helper to build a three-part error message:
///
/// ```text
/// error: <problem description>
///   → <triggering cause>
///   → <suggested fix command or action>
/// ```
///
/// Multi-line `cause`/`fix` blocks have their continuation lines indented
/// past the `→` so embedded text (e.g. captured `git` stderr) reads as
/// nested rather than flush-left.
pub fn three_part(problem: &str, cause: &str, fix: &str) -> String {
    format!(
        "{problem}\n  → {}\n  → {}",
        indent_continuation(cause),
        indent_continuation(fix),
    )
}

fn indent_continuation(s: &str) -> String {
    s.replace('\n', "\n    ")
}

impl From<std::io::Error> for SkmError {
    fn from(e: std::io::Error) -> Self {
        SkmError::io(format!("io error: {e}"))
    }
}
