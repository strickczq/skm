//! Configuration directories and scope / workspace resolution.

use std::path::{Path, PathBuf};

use crate::error::{Result, SkmError, three_part};
use crate::sys::cache;

/// The two manifest scopes. `Project.base` is the manifest's directory,
/// not the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project { base: PathBuf },
}

/// Resolved set of paths a command operates on. Reduces repeated derivation.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub scope: Scope,
    /// Directory holding `skm.toml` / `skm.lock`.
    pub scope_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub lock_path: PathBuf,
    /// Process-mutex flock path: always under `<cache>/flocks/`:
    ///   global → `global.flock`, project → `<sha256(scope_dir)>.flock`
    pub flock_path: PathBuf,
}

impl Workspace {
    fn from_dir(scope: Scope, dir: PathBuf, flock_path: PathBuf) -> Self {
        Workspace {
            manifest_path: dir.join("skm.toml"),
            lock_path: dir.join("skm.lock"),
            flock_path,
            scope_dir: dir,
            scope,
        }
    }
}

/// Home directory via `dirs::home_dir()`.
pub fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| SkmError::io("could not determine home directory"))
}

/// Render `p` for display in a human-facing sentence, abbreviating the home
/// directory to `~`. Falls back to the full path when home is unknown or `p`
/// lies outside it. Matches on path components (via `strip_prefix`), so a
/// sibling like `/home/userX` is never mistaken for a child of `/home/user`.
/// Not for machine-consumed output (e.g. `cache dir`), which must stay literal.
pub fn display_path(p: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}

/// Global config directory `<home>/.config/skm/` or `$SKM_CONFIG_DIR`.
///
/// Built from `home_dir()`, deliberately **not** `dirs::config_dir()`: on macOS
/// that resolves to `~/Library/Application Support`, where a CLI tool should not
/// hide its files. The env override keeps CI / read-only rootfs relocatable.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("SKM_CONFIG_DIR") {
        return Ok(PathBuf::from(v));
    }
    Ok(home_dir()?.join(".config").join("skm"))
}

/// Global cache directory `<home>/.cache/skm/` or `$SKM_CACHE_DIR`.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("SKM_CACHE_DIR") {
        return Ok(PathBuf::from(v));
    }
    Ok(home_dir()?.join(".cache").join("skm"))
}

/// Resolve the workspace for the requested scope.
///
/// * `global` → `<config_dir>`.
/// * project → search upward from CWD for the nearest `skm.toml`, skipping the
///   global config dir. Errors if none found.
pub fn resolve_workspace(global: bool) -> Result<Workspace> {
    if global {
        let dir = config_dir()?;
        let flock = global_flock_path()?;
        return Ok(Workspace::from_dir(Scope::Global, dir, flock));
    }
    let cwd = std::env::current_dir()
        .map_err(|e| SkmError::io(format!("cannot determine current directory: {e}")))?;
    match find_project_manifest_dir(&cwd)? {
        Some(dir) => {
            let flock = project_flock_path(&dir)?;
            Ok(Workspace::from_dir(
                Scope::Project { base: dir.clone() },
                dir,
                flock,
            ))
        }
        None => Err(SkmError::general(three_part(
            "error: no skm.toml found",
            "no manifest exists in this directory or any parent",
            "Run `skm init` to create one.",
        ))),
    }
}

/// Like [`resolve_workspace`] but does not require an existing manifest. Used by
/// `init` and `add` which may create one. For project scope the manifest dir is
/// the located one if any, else CWD.
pub fn resolve_workspace_for_create(global: bool) -> Result<Workspace> {
    if global {
        let dir = config_dir()?;
        let flock = global_flock_path()?;
        return Ok(Workspace::from_dir(Scope::Global, dir, flock));
    }
    let cwd = std::env::current_dir()
        .map_err(|e| SkmError::io(format!("cannot determine current directory: {e}")))?;
    let dir = find_project_manifest_dir(&cwd)?.unwrap_or(cwd);
    let flock = project_flock_path(&dir)?;
    Ok(Workspace::from_dir(
        Scope::Project { base: dir.clone() },
        dir,
        flock,
    ))
}

/// The global scope flock lives alongside project flocks under the cache
/// directory: `<cache>/flocks/global.flock`.
fn global_flock_path() -> Result<PathBuf> {
    Ok(cache_dir()?.join("flocks").join("global.flock"))
}

/// Derive a stable, unique flock path for a project scope under the cache
/// directory. The path is `<cache>/flocks/<sha256(abs_scope_dir)>.flock`.
fn project_flock_path(scope_dir: &Path) -> Result<PathBuf> {
    let hash = cache::sha256_hex_bytes(scope_dir.to_string_lossy().as_bytes());
    Ok(cache_dir()?.join("flocks").join(format!("{hash}.flock")))
}

/// Search upward from `start` for the nearest directory containing `skm.toml`,
/// skipping the global config dir.
pub fn find_project_manifest_dir(start: &Path) -> Result<Option<PathBuf>> {
    // Canonicalize so the macOS /var → /private/var symlink doesn't defeat the
    // config-dir skip guard (same root cause as the pollution guard in add.rs).
    let skm_config = config_dir()
        .ok()
        .and_then(|c| std::fs::canonicalize(&c).ok());
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let is_config_dir = match &skm_config {
            Some(cfg) => std::fs::canonicalize(dir).is_ok_and(|d| d == *cfg),
            None => false,
        };
        if !is_config_dir && dir.join("skm.toml").is_file() {
            return Ok(Some(dir.to_path_buf()));
        }
        cur = dir.parent();
    }
    Ok(None)
}
