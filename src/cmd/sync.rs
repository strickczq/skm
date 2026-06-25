//! `skm sync`.

use crate::error::{ExitCode, Result, SkmError, three_part};
use crate::model::config::{self, Workspace};
use crate::model::lockfile::{self, Lockfile};
use crate::model::manifest;
use crate::pipeline::deploy::{self, DeployOptions};
use crate::pipeline::resolve::{self, LockOptions, Upgrade};
use crate::sys::cache::Cache;

pub struct SyncArgs {
    pub global: bool,
    pub frozen: bool,
    pub locked: bool,
    pub dry_run: bool,
    pub offline: bool,
    pub prune: bool,
    pub no_prune: bool,
    pub yes: bool,
    /// Re-resolution policy for the lock phase. `skm sync` always passes
    /// [`Upgrade::None`]; `skm update` sets this to upgrade mutable refs.
    pub upgrade: Upgrade,
}

pub fn run(args: SyncArgs) -> Result<ExitCode> {
    let ws = config::resolve_workspace(args.global)?;
    let _guard = super::lock_exclusive(&ws)?;
    super::gc_scope(&ws.scope)?;
    execute(&ws, &args)
}

/// Core sync, assuming the scope lock is held and GC already ran. Shared with
/// `add`.
pub fn execute(ws: &Workspace, args: &SyncArgs) -> Result<ExitCode> {
    let manifest = manifest::load(&ws.manifest_path)?;
    let prev_lock_opt = lockfile::load(&ws.lock_path)?;
    let cache = Cache::open(config::cache_dir()?)?;

    let prune_effective = if args.no_prune {
        false
    } else if args.prune {
        true
    } else {
        !args.frozen
    };

    let frozen_or_locked = args.frozen || args.locked;

    // Ownership snapshot (lock at sync start) for the foreign guard.
    let prev_lock = prev_lock_opt.clone().unwrap_or_else(Lockfile::empty);

    let mut deploy_lock: Lockfile;
    let write_lock: bool;
    // Names deferred by an offline dry-run: identity changed but no network.
    let mut deferred: Vec<String> = Vec::new();

    if frozen_or_locked {
        let lock = prev_lock_opt.clone().ok_or_else(|| {
            SkmError::lock_missing(three_part(
                "error: skm.lock is required for --frozen/--locked",
                "no lockfile exists",
                "Run `skm sync` (without --frozen/--locked) or `skm lock` first.",
            ))
        })?;
        if args.locked {
            deploy::locked_determination(&manifest, &ws.scope_dir, &lock)?;
        }
        // Each manifest skill must be present in the lock under --frozen.
        for skill in &manifest.skills {
            if lock.get(&skill.name).is_none() {
                return Err(SkmError::mismatch_abort(format!(
                    "error: skill '{}' is in manifest but not in lock; run 'skm lock' first or use 'skm sync' without --frozen",
                    skill.name
                )));
            }
        }
        deploy_lock = lock;
        write_lock = false;
    } else {
        // Normal / dry-run: run the lock phase.
        let existing = prev_lock_opt.clone().unwrap_or_else(Lockfile::empty);
        let opts = LockOptions {
            upgrade: args.upgrade.clone(),
            offline: args.offline,
            dry_run: args.dry_run,
        };
        let (new_lock, _changed, d) = resolve::lock_phase(
            &manifest,
            &ws.scope_dir,
            &ws.scope,
            &existing,
            &cache,
            &opts,
        )?;
        deferred = d;
        deploy_lock = new_lock;
        // Carry over owned entries not in the manifest (managed extras) so prune
        // can identify ownership; ghost cleanup later drops residue-free ones.
        let manifest_names: Vec<&str> = manifest.skills.iter().map(|s| s.name.as_str()).collect();
        super::carry_over_managed_extras(&mut deploy_lock, &existing, &manifest_names);
        write_lock = !args.dry_run;
    }

    let deploy_opts = DeployOptions {
        frozen: args.frozen,
        locked: args.locked,
        dry_run: args.dry_run,
        offline: args.offline,
        prune: prune_effective,
        yes: args.yes,
        offline_deferred: deferred,
    };

    let outcome = deploy::deploy(
        &manifest,
        &ws.scope_dir,
        &ws.scope,
        &mut deploy_lock,
        &prev_lock,
        &cache,
        &deploy_opts,
    )?;

    if args.dry_run {
        outcome.plan.print();
        return Ok(outcome.exit);
    }

    if write_lock {
        let needs_write = match &prev_lock_opt {
            None => true,
            Some(prev) => !lockfile::content_eq(prev, &deploy_lock),
        };
        if needs_write {
            lockfile::write(&ws.lock_path, &deploy_lock)?;
        }
    }

    super::print_change_summary(&outcome.plan);
    Ok(outcome.exit)
}
