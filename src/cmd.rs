//! CLI subcommand implementations.

pub mod add;
pub mod cache;
pub mod doctor;
pub mod init;
pub mod lock;
pub mod remove;
pub mod status;
pub mod sync;
pub mod update;

use crate::error::{Result, SkmError};
use crate::model::agent::Agent;
use crate::model::config::{Scope, Workspace};
use crate::model::lockfile::Lockfile;
use crate::pipeline::deploy::Plan;
use crate::pipeline::resolve::Upgrade;
use crate::sys::fsutil::{self, FileLock};
use crate::ui;

/// Report ghost (orphan lock entry) cleanup in a sync/add summary. Shared so a
/// ghost-only convergence is never silent (a plan that *only* dropped stale
/// lock entries must still print something).
pub fn report_ghost(plan: &Plan) {
    let n = plan.ghost.len();
    if n > 0 {
        ui::ok!(
            "Cleaned {n} stale lock entr{}.",
            if n == 1 { "y" } else { "ies" }
        );
    }
}

/// Pick the singular or plural noun for a count: `noun(1, "agent",
/// "agents")`. Keeps user-facing counts grammatical instead of `agent(s)`.
pub fn noun(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// Summarize a converged (non-dry-run) deploy plan for `sync`/`add`. Shared so
/// both commands phrase install/repair/prune/ghost identically.
pub fn print_change_summary(plan: &Plan) {
    let changed = plan.install.len() + plan.repair.len();
    if changed == 0 && plan.remove.is_empty() && plan.ghost.is_empty() {
        ui::say!("Already up to date.");
        return;
    }
    if !plan.install.is_empty() {
        let n = plan.install.len();
        ui::ok!("Installed {n} {}.", noun(n, "agent", "agents"));
    }
    if !plan.repair.is_empty() {
        let n = plan.repair.len();
        ui::ok!("Repaired {n} {}.", noun(n, "agent", "agents"));
    }
    if !plan.remove.is_empty() {
        let n = plan.remove.len();
        ui::ok!("Pruned {n} managed {}.", noun(n, "extra", "extras"));
    }
    report_ghost(plan);
}

/// Acquire the scope's exclusive lock (write commands). Exit code 5 on
/// failure.
pub fn lock_exclusive(ws: &Workspace) -> Result<FileLock> {
    fsutil::acquire_lock(&ws.flock_path, true, false).map_err(|_| {
        SkmError::io(format!(
            "could not acquire scope lock '{}'; another skm command may be running",
            ws.flock_path.display()
        ))
    })
}

/// Acquire the scope's shared lock (read-only commands).
pub fn lock_shared(ws: &Workspace) -> Result<FileLock> {
    fsutil::acquire_lock(&ws.flock_path, false, false).map_err(|_| {
        SkmError::io(format!(
            "could not acquire scope lock '{}'; another skm command may be running",
            ws.flock_path.display()
        ))
    })
}

/// Crash-recovery GC over every agent root of the scope. Only for write
/// commands holding `LOCK_EX`.
pub fn gc_scope(scope: &Scope) -> Result<()> {
    for a in Agent::ALL {
        let root = a.skills_root(scope)?;
        fsutil::gc_skills_root(&root)?;
    }
    Ok(())
}

/// Convert clap's optional upgrade vector into [`Upgrade`].
pub fn upgrade_from_opt(opt: Option<Vec<String>>) -> Upgrade {
    match opt {
        None => Upgrade::None,
        Some(v) if v.is_empty() => Upgrade::All,
        Some(v) => Upgrade::Named(v),
    }
}

/// Carry over lock entries for skills that are owned (in `prev_lock`) but no
/// longer declared in the manifest.  These "managed extras" must stay in the
/// deploy lock so prune can identify ownership; ghost cleanup later drops any
/// that have no on-disk residue.  Shared by `sync` and `add`.
pub fn carry_over_managed_extras(
    deploy_lock: &mut Lockfile,
    prev_lock: &Lockfile,
    manifest_names: &[&str],
) {
    for e in &prev_lock.skills {
        if !manifest_names.contains(&e.name.as_str()) && deploy_lock.get(&e.name).is_none() {
            deploy_lock.skills.push(e.clone());
        }
    }
    deploy_lock.sort();
}
