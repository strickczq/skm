//! `skm remove`.

use crate::error::{ExitCode, Result, SkmError};
use crate::model::agent::{self, Agent};
use crate::model::config;
use crate::model::lockfile;
use crate::model::manifest;
use crate::sys::fsutil;
use crate::ui;

/// Order is manifest → disk → lock (ownership removed **last**): if disk deletion
/// fails, ownership stays recorded in the lock and the operation is retryable.
/// `--no-sync` is intentionally asymmetric with `add --no-sync` — it touches
/// neither the lock nor the disk, deferring all deletion to the next sync's prune.
pub fn run(global: bool, names: Vec<String>, no_sync: bool) -> Result<ExitCode> {
    let ws = config::resolve_workspace(global)?;
    let _guard = super::lock_exclusive(&ws)?;
    super::gc_scope(&ws.scope)?;

    let manifest = manifest::load(&ws.manifest_path)?;
    for name in &names {
        if !manifest.skills.iter().any(|s| &s.name == name) {
            return Err(SkmError::general(format!(
                "error: skill '{name}' not found in manifest\n  → '{name}' is not declared in skm.toml\n  → Run `skm status` to see available skills."
            )));
        }
    }

    // 1. Ownership snapshot from the pre-operation lock.
    let lock_opt = lockfile::load(&ws.lock_path)?;

    // 2. Modify the manifest (delete entries).
    let mut doc = manifest::load_doc(&ws.manifest_path)?;
    for name in &names {
        manifest::remove_skill(&mut doc, name);
    }
    manifest::save_doc(&ws.manifest_path, &doc)?;
    ui::ok!(
        "Removed {} from {}",
        names.join(", "),
        config::display_path(&ws.manifest_path)
    );

    if no_sync {
        ui::say!("Manifest updated; run `skm sync` to prune from disk and lock.");
        return Ok(ExitCode::Success);
    }

    let Some(mut lock) = lock_opt else {
        // No lock → nothing owned to delete; nothing to update.
        ui::say!("No lockfile; nothing to deploy.");
        return Ok(ExitCode::Success);
    };

    // 3. Delete owned (managed) directories from disk — only from the agent
    //    roots the manifest declares for this skill. Deploy never touches roots
    //    outside the declared set, so remove must not either: a same-named
    //    directory in an unrelated root is foreign and must be preserved.
    let mut removed_dirs = 0usize;
    for name in &names {
        let owned = lock.get(name).is_some();
        if !owned {
            ui::note!(
                "'{name}' is not owned by skm (foreign); leaving any on-disk directory untouched."
            );
            continue;
        }
        // Determine which agent roots this skill was deployed to.
        let agents: Vec<Agent> = match manifest.skills.iter().find(|s| &s.name == name) {
            Some(skill) => {
                let ids = skill.effective_agents(&manifest.default_agents)?;
                agent::parse_agents(&ids)?
            }
            // Skill is in the lock but was already removed from the manifest
            // (partial-failure recovery): fall back to all roots to avoid
            // leaving orphan dirs — same as the pre-fix behaviour.
            None => Agent::ALL.to_vec(),
        };
        for a in &agents {
            let root = a.skills_root(&ws.scope)?;
            let final_path = root.join(name);
            if final_path.is_dir() {
                fsutil::atomic_remove_dir(&root, name)?;
                removed_dirs += 1;
            }
        }
    }

    // 4. Update the lock (delete entries, atomic write).
    lock.skills.retain(|s| !names.contains(&s.name));
    lock.sort();
    lockfile::write(&ws.lock_path, &lock)?;

    // Report the disk/lock side effects so the prune isn't silent.
    if removed_dirs > 0 {
        ui::ok!(
            "Pruned {removed_dirs} {} and updated lock.",
            super::noun(removed_dirs, "directory", "directories")
        );
    } else {
        ui::ok!("Updated lock.");
    }
    Ok(ExitCode::Success)
}
