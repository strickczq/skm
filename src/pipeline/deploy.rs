//! Deploy phase and atomic landing. Lock-driven: content and
//! ownership come from the lock; the manifest only supplies agents.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::error::{ExitCode, Result, SkmError, three_part};
use crate::model::agent::{Agent, parse_agents};
use crate::model::config::{self, Scope};
use crate::model::lockfile::{LockSkill, Lockfile};
use crate::model::manifest::Manifest;
use crate::pipeline::resolve;
use crate::source::SourceType;
use crate::sys::cache::Cache;
use crate::sys::fsutil;
use crate::sys::hash::{self, Sha256};
use crate::ui;

/// The change plan (also the `--dry-run` output).
#[derive(Debug, Default)]
pub struct Plan {
    pub install: Vec<(String, Agent)>,
    pub repair: Vec<(String, Agent)>,
    pub remove: Vec<(String, Agent)>,
    pub lock_updates: Vec<String>,
    pub ghost: Vec<String>,
    /// Under `--frozen` / `--locked` + `--dry-run`: skills whose freshly
    /// materialized content_sha256 disagrees with the lock. The real
    /// `--frozen` run would abort here (exit 3).
    pub frozen_drift: Vec<String>,
}

impl Plan {
    pub fn has_changes(&self) -> bool {
        !self.install.is_empty()
            || !self.repair.is_empty()
            || !self.remove.is_empty()
            || !self.lock_updates.is_empty()
            || !self.ghost.is_empty()
    }

    pub fn print(&self) {
        let print_section = |title: &str, items: &[(String, Agent)]| {
            if !items.is_empty() {
                ui::heading!("{title}");
                for (n, a) in items {
                    ui::say!("  {n} → {a}");
                }
            }
        };
        print_section("Install:", &self.install);
        print_section("Remove (managed extra):", &self.remove);
        print_section("Repair (drift):", &self.repair);
        if !self.lock_updates.is_empty() {
            ui::heading!("Lock (will update):");
            for u in &self.lock_updates {
                ui::say!("  {u}");
            }
        }
        if !self.ghost.is_empty() {
            ui::heading!("Ghost (lock cleanup):");
            for g in &self.ghost {
                ui::say!("  {g}");
            }
        }
        if !self.frozen_drift.is_empty() {
            ui::heading!("Frozen drift (would abort):");
            for n in &self.frozen_drift {
                ui::say!("  {n} (content_sha256 ≠ lock)");
            }
        }
        // Unchanged (noop) entries are intentionally omitted: a change plan
        // should surface only what changes.
    }
}

/// Direct child directory names of a skills root, skipping dotfiles and temp
/// dirs (scan rule).
pub fn list_root_dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || fsutil::is_temp_name(&name) {
                continue;
            }
            if entry.path().is_dir() {
                out.push(name);
            }
        }
    }
    out
}

/// Map each agent variant to its skills root under the scope (scan scope).
fn all_roots(scope: &Scope) -> Result<Vec<(Agent, PathBuf)>> {
    let mut v = Vec::new();
    for a in Agent::ALL {
        v.push((a, a.skills_root(scope)?));
    }
    Ok(v)
}

pub struct DeployOptions {
    pub frozen: bool,
    pub locked: bool,
    pub dry_run: bool,
    pub offline: bool,
    /// Effective prune decision (already resolves --prune/--no-prune defaults).
    pub prune: bool,
    pub yes: bool,
    /// Names deferred by an offline dry-run: identity changed but no network
    /// to re-resolve. Reported under `LOCK (may update)`; deploy skips them.
    pub offline_deferred: Vec<String>,
}

/// `--locked` determination: lock ≡ manifest.
pub fn locked_determination(
    manifest: &Manifest,
    manifest_dir: &Path,
    lock: &Lockfile,
) -> Result<()> {
    let manifest_names: BTreeSet<&str> = manifest.skills.iter().map(|s| s.name.as_str()).collect();
    let lock_names: BTreeSet<&str> = lock.skills.iter().map(|s| s.name.as_str()).collect();
    if manifest_names != lock_names {
        return Err(SkmError::mismatch_abort(three_part(
            "error: lock does not match manifest (--locked)",
            "the set of skill names differs between skm.toml and skm.lock",
            "Run `skm lock` to update the lockfile.",
        )));
    }
    for skill in &manifest.skills {
        // agents must be non-empty.
        skill.effective_agents(&manifest.default_agents)?;
        let lock_entry = lock.get(&skill.name).ok_or_else(|| {
            SkmError::general("internal: lock entry missing after name set match")
        })?;
        if !resolve::identity_matches(&skill.source, manifest_dir, &lock_entry.source)? {
            return Err(SkmError::mismatch_abort(three_part(
                &format!(
                    "error: lock does not match manifest for '{}' (--locked)",
                    skill.name
                ),
                "the source identity in skm.toml differs from skm.lock",
                "Run `skm lock` to update the lockfile.",
            )));
        }
    }
    Ok(())
}

/// Materialize a lock entry into staging and return (content_root, staging_base).
///
/// `frozen_lock_context` is forwarded so an offline cache miss under
/// `--frozen`/`--locked` reports [`ExitCode::LockMissing`] (6) per the
/// `--offline` table.
fn materialize_lock_entry(
    entry: &LockSkill,
    manifest_dir: &Path,
    cache: &Cache,
    offline: bool,
    frozen_lock_context: bool,
) -> Result<(PathBuf, PathBuf)> {
    let source = entry.source.to_source();
    let resolved = resolve::resolved_from_lock(&entry.source);
    let staging_base = cache.new_staging(&entry.name)?;
    match source.materialize(
        cache,
        manifest_dir,
        &resolved,
        offline,
        frozen_lock_context,
        &staging_base,
    ) {
        Ok(content_root) => Ok((content_root, staging_base)),
        Err(e) => {
            let _ = fsutil::remove_dir_all(&staging_base);
            Err(e)
        }
    }
}

/// Per-skill landing context. Everything except the destination `root`
/// is stable across the skill's agents, so landing is a method on this.
struct Landing<'a> {
    content: &'a Path,
    name: &'a str,
    source_type: SourceType,
    prev_owned: &'a HashSet<String>,
    frozen_or_locked: bool,
    hash: Sha256,
}

impl Landing<'_> {
    /// Atomic landing of the content tree into `root/name`.
    fn land(&self, root: &Path) -> Result<()> {
        std::fs::create_dir_all(root)
            .map_err(|e| SkmError::io(format!("cannot create '{}': {e}", root.display())))?;
        let final_path = root.join(self.name);
        let new_at = fsutil::new_path(root, self.name);
        // copy_tree, never rename: every agent of this skill reuses the same
        // staging tree, so consuming it with a rename would starve the next one.
        fsutil::copy_tree(self.content, &new_at)?;

        if final_path.exists() {
            let managed = self.prev_owned.contains(self.name);
            if !managed {
                // Foreign guard (step 2).
                if self.frozen_or_locked {
                    let _ = fsutil::remove_dir_all(&new_at);
                    return Err(SkmError::mismatch_abort(three_part(
                        &format!(
                            "error: refusing to overwrite foreign directory '{}'",
                            final_path.display()
                        ),
                        "the directory is not owned by skm and --frozen/--locked is set",
                        "Remove it manually (`rm -r`) then retry, or run sync without --frozen.",
                    )));
                }
                let disk_hash = hash::hash_tree(&final_path, self.source_type.exec_policy());
                match disk_hash {
                    Ok(h) if h == self.hash => { /* identical content → allow overwrite */ }
                    Ok(_) => {
                        let _ = fsutil::remove_dir_all(&new_at);
                        return Err(SkmError::mismatch_abort(three_part(
                            &format!(
                                "error: refusing to overwrite foreign directory '{}'",
                                final_path.display()
                            ),
                            "a directory not installed by skm already exists with different content",
                            "Remove it manually (`rm -r`) then retry.",
                        )));
                    }
                    Err(e) => {
                        let _ = fsutil::remove_dir_all(&new_at);
                        return Err(SkmError::mismatch_abort(three_part(
                            &format!(
                                "error: cannot inspect foreign directory '{}'",
                                final_path.display()
                            ),
                            &format!("unable to hash the existing directory: {e}"),
                            "Check permissions and try again, or remove the directory manually (`rm -r`) then retry.",
                        )));
                    }
                }
            }
            let old_at = fsutil::old_path(root, self.name);
            fsutil::rename(&final_path, &old_at)?;
            if let Err(e) = fsutil::rename(&new_at, &final_path) {
                // Try to restore the previous version.
                let _ = fsutil::rename(&old_at, &final_path);
                return Err(e);
            }
            fsutil::remove_dir_all(&old_at)?;
        } else {
            fsutil::rename(&new_at, &final_path)?;
        }
        Ok(())
    }
}

/// Result of a sync run.
pub struct SyncOutcome {
    pub plan: Plan,
    pub exit: ExitCode,
}

/// Run the full sync (lock phase already applied for normal mode; this is the
/// deploy half plus prune/ghost). `deploy_lock` is the lockfile whose entries
/// drive deployment. `prev_owned` is the set of names owned before this sync
/// (used by the foreign guard). Returns the (possibly mutated) lock via
/// `deploy_lock` and an outcome.
pub fn deploy(
    manifest: &Manifest,
    manifest_dir: &Path,
    scope: &Scope,
    deploy_lock: &mut Lockfile,
    prev_lock: &Lockfile,
    cache: &Cache,
    opts: &DeployOptions,
) -> Result<SyncOutcome> {
    let frozen_or_locked = opts.frozen || opts.locked;
    let prev_owned: HashSet<String> = prev_lock.skills.iter().map(|s| s.name.clone()).collect();

    // Effective agents per manifest skill.
    let mut manifest_agents: BTreeMap<String, Vec<Agent>> = BTreeMap::new();
    for skill in &manifest.skills {
        let ids = skill.effective_agents(&manifest.default_agents)?;
        manifest_agents.insert(skill.name.clone(), parse_agents(&ids)?);
    }
    let manifest_names: BTreeSet<String> = manifest.skills.iter().map(|s| s.name.clone()).collect();

    let roots = all_roots(scope)?;
    let mut plan = Plan::default();

    // Lock-update plan (dry-run information).
    for entry in &deploy_lock.skills {
        if !manifest_names.contains(&entry.name) {
            continue;
        }
        match prev_lock.get(&entry.name) {
            None => plan
                .lock_updates
                .push(format!("{} (new entry)", entry.name)),
            Some(prev) if prev.content_sha256 != entry.content_sha256 => plan
                .lock_updates
                .push(format!("{} (content_sha256 changed)", entry.name)),
            _ => {}
        }
    }
    // Offline dry-run: identity-changed/new entries we could not re-resolve.
    for name in &opts.offline_deferred {
        plan.lock_updates.push(format!(
            "{name} (may update — offline, source identity changed)"
        ));
    }

    // Per-skill deploy. Iterate manifest skills (they must be in the lock).
    for name in &manifest_names {
        let Some(entry) = deploy_lock.get(name).cloned() else {
            // Offline dry-run deferred a brand-new skill: no lock entry yet and we
            // cannot resolve it offline. It is already reported under LOCK (may
            // update); skip deploy actions for it.
            if opts.offline_deferred.iter().any(|n| n == name) {
                continue;
            }
            return Err(SkmError::general(format!(
                "error: skill '{name}' is in manifest but not in lock; run 'skm lock' first or use 'skm sync' without --frozen"
            )));
        };
        let agents = manifest_agents.get(name).cloned().unwrap_or_default();
        let source_type = entry.source.source_type();

        // Decide per-agent action.
        let mut to_land: Vec<Agent> = Vec::new();
        for a in &agents {
            let root = a.skills_root(scope)?;
            let final_path = root.join(name);
            if !final_path.exists() {
                plan.install.push((name.clone(), *a));
                to_land.push(*a);
            } else {
                let disk = hash::hash_tree(&final_path, source_type.exec_policy());
                match disk {
                    Ok(h) if h == entry.content_sha256 => {}
                    _ => {
                        plan.repair.push((name.clone(), *a));
                        to_land.push(*a);
                    }
                }
            }
        }

        // Under --frozen/--locked we must materialize and verify every entry
        // against the lock (the content gate) even when no agent needs
        // landing — this is how local-source drift is caught. Under normal sync
        // the lock phase already verified, so noop entries can be skipped.
        // Under --dry-run we still materialize for frozen/locked so the plan
        // honestly reflects "would the real --frozen abort?".
        let landing_needed = !opts.dry_run && !to_land.is_empty();
        let verify_needed = frozen_or_locked;
        if !landing_needed && !verify_needed {
            continue;
        }

        // Materialize once, verify, land to each needed agent.
        let (content_root, staging_base) =
            materialize_lock_entry(&entry, manifest_dir, cache, opts.offline, frozen_or_locked)?;
        let exec = (|| {
            let staging_hash = hash::hash_tree(&content_root, source_type.exec_policy())?;
            if frozen_or_locked && staging_hash != entry.content_sha256 {
                if opts.dry_run {
                    // Record and continue so the plan reports every drift
                    // before exiting.
                    plan.frozen_drift.push(name.clone());
                    return Ok(());
                }
                return Err(SkmError::mismatch_abort(three_part(
                    &format!("error: content for '{name}' does not match the lock"),
                    "the materialized content_sha256 differs from skm.lock",
                    "Run `skm lock` to update, or investigate the source.",
                )));
            }
            if opts.dry_run {
                return Ok(());
            }
            let landing = Landing {
                content: &content_root,
                name,
                source_type,
                prev_owned: &prev_owned,
                frozen_or_locked,
                hash: staging_hash,
            };
            for a in &to_land {
                let root = a.skills_root(scope)?;
                landing.land(&root)?;
            }
            Ok(())
        })();
        let _ = fsutil::remove_dir_all(&staging_base);
        exec?;
    }

    // ---- Snapshot disk existence across all roots BEFORE prune ----
    let mut residue: BTreeMap<String, Vec<Agent>> = BTreeMap::new();
    for (a, root) in &roots {
        for dir in list_root_dirs(root) {
            residue.entry(dir).or_default().push(*a);
        }
    }

    // ---- E: prune managed extras ----
    // A directory is a managed extra if it is owned (in deploy_lock) but the
    // manifest does not declare that (name, agent).
    // Only build the prune plan when prune is effective (--no-prune / the
    // --frozen default disable it). Otherwise `plan.remove` would advertise a
    // removal that never happens — a false "Pruned N" summary / dry-run REMOVE.
    let mut prune_agents: Vec<(String, Agent, PathBuf)> = Vec::new();
    if opts.prune {
        for (a, root) in &roots {
            for dir in list_root_dirs(root) {
                let owned = deploy_lock.get(&dir).is_some();
                if !owned {
                    continue; // foreign — never touched
                }
                let declared_here = manifest_agents
                    .get(&dir)
                    .map(|ts| ts.contains(a))
                    .unwrap_or(false);
                if !declared_here {
                    prune_agents.push((dir.clone(), *a, root.join(&dir)));
                    plan.remove.push((dir, *a));
                }
            }
        }
    }

    // Distinguish a non-interactive "needs --yes" (a hard error) from an
    // interactive decline (skip prune and finish successfully).
    let mut prune_needs_yes = false;
    if !opts.dry_run && opts.prune && !prune_agents.is_empty() {
        match confirm_prune(&prune_agents, opts.yes)? {
            PruneDecision::Proceed => {
                for (name, a, _path) in &prune_agents {
                    let root = a.skills_root(scope)?;
                    fsutil::atomic_remove_dir(&root, name)?;
                }
            }
            PruneDecision::Declined => {
                // User answered "N" at the prompt: leave the extras in place and
                // drop them from the plan so the summary stays honest.
                ui::note!(
                    "Skipped prune; {} managed extra(s) left in place.",
                    prune_agents.len()
                );
                plan.remove.clear();
            }
            PruneDecision::NeedsConfirmation => {
                prune_needs_yes = true;
            }
        }
    }

    // ---- D: ghost ownership cleanup (uses pre-prune snapshot) ----
    // Names in lock, not in manifest, with no disk residue (snapshot) → drop.
    let lock_names: Vec<String> = deploy_lock.skills.iter().map(|s| s.name.clone()).collect();
    for name in lock_names {
        if manifest_names.contains(&name) {
            continue;
        }
        let had_residue = residue.get(&name).map(|v| !v.is_empty()).unwrap_or(false);
        if !had_residue {
            if frozen_or_locked {
                ui::warn!("orphan lock entry '{name}'; run 'skm sync' to clean up");
            } else {
                plan.ghost.push(name.clone());
                deploy_lock.skills.retain(|s| s.name != name);
            }
        }
    }

    deploy_lock.sort();

    let exit = if opts.dry_run {
        if !plan.frozen_drift.is_empty() {
            // Predicts a real --frozen/--locked abort (exit code 3).
            ExitCode::MismatchAbort
        } else if plan.has_changes() {
            ExitCode::DryRunChanges
        } else {
            ExitCode::Success
        }
    } else if prune_needs_yes {
        return Err(SkmError::general(
            "prune requires --yes in non-interactive mode",
        ));
    } else {
        ExitCode::Success
    };

    Ok(SyncOutcome { plan, exit })
}

/// Outcome of the prune confirmation. `NeedsConfirmation` is the
/// non-interactive case the caller turns into the documented `--yes` error;
/// `Declined` is an interactive "N" which simply skips prune.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PruneDecision {
    Proceed,
    Declined,
    NeedsConfirmation,
}

/// Pure decision logic, separated from IO so it can be unit-tested. `answer` is
/// the line read from the prompt (only consulted when interactive).
fn decide_prune(yes: bool, is_tty: bool, answer: Option<&str>) -> PruneDecision {
    if yes {
        return PruneDecision::Proceed;
    }
    if !is_tty {
        return PruneDecision::NeedsConfirmation;
    }
    match answer {
        Some(a) if matches!(a.trim(), "y" | "Y" | "yes" | "Yes") => PruneDecision::Proceed,
        _ => PruneDecision::Declined,
    }
}

/// Confirm a prune operation, prompting interactively when needed.
fn confirm_prune(agents: &[(String, Agent, PathBuf)], yes: bool) -> Result<PruneDecision> {
    let is_tty = std::io::stdin().is_terminal();
    if yes || !is_tty {
        return Ok(decide_prune(yes, is_tty, None));
    }
    ui::say!("The following managed directories will be removed:");
    for (_, _, path) in agents {
        ui::say!("  {}  (not in manifest)", config::display_path(path));
    }
    ui::print!("Proceed? [y/N]: ");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| SkmError::io(format!("cannot read confirmation: {e}")))?;
    Ok(decide_prune(yes, is_tty, Some(&line)))
}

#[cfg(test)]
mod tests {
    use super::{PruneDecision, decide_prune};

    #[test]
    fn yes_flag_always_proceeds() {
        // --yes proceeds regardless of TTY/answer.
        assert_eq!(decide_prune(true, false, None), PruneDecision::Proceed);
        assert_eq!(decide_prune(true, true, Some("n")), PruneDecision::Proceed);
    }

    #[test]
    fn non_interactive_needs_confirmation() {
        // No --yes and no TTY → the documented "needs --yes" error path.
        assert_eq!(
            decide_prune(false, false, None),
            PruneDecision::NeedsConfirmation
        );
    }

    #[test]
    fn interactive_decline_is_not_an_error() {
        // Interactive "N" (or anything but yes) skips prune; it must NOT collapse
        // into the non-interactive NeedsConfirmation case.
        assert_eq!(
            decide_prune(false, true, Some("n\n")),
            PruneDecision::Declined
        );
        assert_eq!(decide_prune(false, true, Some("")), PruneDecision::Declined);
        assert_eq!(
            decide_prune(false, true, Some("nope")),
            PruneDecision::Declined
        );
    }

    #[test]
    fn interactive_accept_proceeds() {
        for a in ["y", "Y", "yes", "Yes", " y \n"] {
            assert_eq!(
                decide_prune(false, true, Some(a)),
                PruneDecision::Proceed,
                "answer {a:?} should proceed"
            );
        }
    }
}
