//! `skm lock`.

use crate::error::{ExitCode, Result, SkmError};
use crate::model::config;
use crate::model::lockfile::{self, Lockfile};
use crate::model::manifest;
use crate::pipeline::resolve::{self, LockOptions, Upgrade};
use crate::sys::cache::Cache;
use crate::ui;

pub fn run(global: bool, upgrade: Option<Vec<String>>) -> Result<ExitCode> {
    let ws = config::resolve_workspace(global)?;
    let _guard = super::lock_exclusive(&ws)?;
    super::gc_scope(&ws.scope)?;

    let manifest = manifest::load(&ws.manifest_path)?;
    let existing = lockfile::load(&ws.lock_path)?.unwrap_or_else(Lockfile::empty);

    let upgrade = super::upgrade_from_opt(upgrade);
    // Validate named upgrades exist.
    if let Upgrade::Named(names) = &upgrade {
        for n in names {
            if !manifest.skills.iter().any(|s| &s.name == n) {
                return Err(SkmError::general(format!(
                    "error: cannot upgrade '{n}': not declared in skm.toml"
                )));
            }
        }
    }

    let cache = Cache::open(config::cache_dir()?)?;
    let opts = LockOptions {
        upgrade,
        offline: false,
        dry_run: false,
    };
    let (new_lock, changed, _deferred) = resolve::lock_phase(
        &manifest,
        &ws.scope_dir,
        &ws.scope,
        &existing,
        &cache,
        &opts,
    )?;

    if changed || lockfile::load(&ws.lock_path)?.is_none() {
        lockfile::write(&ws.lock_path, &new_lock)?;
        ui::ok!("Wrote {}", config::display_path(&ws.lock_path));
        // Surface what each newly-locked / re-pinned entry resolved to, so the
        // user sees the version without opening skm.lock.
        for s in &new_lock.skills {
            if existing.get(&s.name) != Some(s) {
                ui::say!("  {} → {}", s.name, s.source.summary());
            }
        }
    } else {
        ui::say!("Lock is up to date.");
    }
    Ok(ExitCode::Success)
}
