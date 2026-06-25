//! `skm add`: smart spec parsing + manifest edit + lock + sync.
//!
//! The pure spec-parsing / name-inference half lives in [`spec`].

mod smart_spec;

use crate::error::{ExitCode, Result, SkmError, three_part};
use crate::model::agent::{self, parse_agents};
use crate::model::config;
use crate::model::lockfile::{self, Lockfile};
use crate::model::manifest::{self, NewEntry};
use crate::pipeline::deploy::{self, DeployOptions};
use crate::pipeline::resolve;
use crate::sys::cache::Cache;
use crate::ui;

#[derive(Clone)]
pub struct AddArgs {
    pub global: bool,
    pub spec: Vec<String>,
    pub subdir: Option<String>,
    pub ref_: Option<String>,
    pub name: Option<String>,
    pub sha256: Option<String>,
    pub agent: Vec<String>,
    pub no_sync: bool,
    pub force: bool,
}

pub fn run(args: AddArgs) -> Result<ExitCode> {
    let ws = config::resolve_workspace_for_create(args.global)?;
    let manifest_exists = ws.manifest_path.is_file();

    // Pollution guard: refuse implicit project manifest in home/root.
    if !args.global && !manifest_exists && !args.force {
        let cwd = std::env::current_dir().map_err(|e| SkmError::io(e.to_string()))?;
        let cwd_canon = std::fs::canonicalize(&cwd).ok();

        // Canonicalize both to survive macOS /var → /private/var symlink
        // divergence: env::current_dir() resolves symlinks, but HOME may
        // not, so a literal `==` returns false for the same directory.
        let home_canon = config::home_dir()
            .ok()
            .and_then(|h| std::fs::canonicalize(&h).ok());

        let is_home = home_canon
            .zip(cwd_canon)
            .map(|(h, c)| h == c)
            .unwrap_or(false);

        let is_root = cwd.parent().is_none();

        if is_home || is_root {
            return Err(SkmError::general(three_part(
                "error: refusing to create a project skm.toml in your home (or filesystem root) directory",
                "no skm.toml was found and CWD is a sensitive directory",
                "Run 'skm init' in a dedicated project folder, or pass --force to create it here.",
            )));
        }
    }

    let parsed = smart_spec::SmartSpec::parse(&args)?;
    let final_name = parsed.infer_name(&args)?;

    // Determine agents to write.
    let agents_to_write: Option<Vec<String>> = if args.agent.is_empty() {
        // Rely on [defaults].agents — verify it exists (else error). The
        // template no longer seeds a default, so a fresh manifest has none:
        // the user must choose explicitly.
        let defaults_present =
            manifest_exists && manifest::load(&ws.manifest_path)?.default_agents.is_some();
        if !defaults_present {
            return Err(SkmError::general(three_part(
                "error: no agents",
                "no --agent was given and skm.toml has no [defaults].agents",
                &format!(
                    "Pass --agent {}, or set [defaults].agents in skm.toml.",
                    agent::agent_flag_choices()
                ),
            )));
        }
        None
    } else {
        Some(
            parse_agents(&args.agent)?
                .iter()
                .map(|a| a.id().to_string())
                .collect(),
        )
    };

    // Re-add policy: if the name already exists with the *same* spec, treat
    // this as idempotent and fall through to lock+sync (which also recovers a
    // prior partial failure). Different spec → error; user must `skm remove`
    // first.
    let mut skip_manifest_edit = false;
    if manifest_exists {
        let m = manifest::load(&ws.manifest_path)?;
        if let Some(existing) = m.skills.iter().find(|s| s.name == final_name) {
            let same_source = existing.source == parsed.source;
            let same_agents = match (&existing.agents_override, &agents_to_write) {
                (Some(a), Some(b)) => a == b,
                // Caller didn't pass --agent → don't compare; rely on existing.
                (_, None) => true,
                // Existing has no override but caller passed --agent → drift.
                (None, Some(_)) => false,
            };
            if same_source && same_agents {
                ui::note!("'{final_name}' already in manifest with matching spec; reconverging.");
                skip_manifest_edit = true;
            } else {
                return Err(SkmError::general(format!(
                    "error: skill '{final_name}' already exists in skm.toml with a different spec\n  → run `skm remove {final_name}` first to replace it"
                )));
            }
        }
        if let Some(other) = m
            .skills
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&final_name) && s.name != final_name)
        {
            ui::warn!(
                "'{final_name}' differs only in case from existing '{}'; ambiguous on macOS.",
                other.name
            );
        }
    }

    if !args.global && !manifest_exists {
        std::fs::create_dir_all(&ws.scope_dir).map_err(|e| {
            SkmError::io(format!("cannot create '{}': {e}", ws.scope_dir.display()))
        })?;
    }

    // Acquire the scope lock now that we know where the manifest lives.
    let _guard = super::lock_exclusive(&ws)?;
    super::gc_scope(&ws.scope)?;

    let cache = Cache::open(config::cache_dir()?)?;
    let prev_lock = lockfile::load(&ws.lock_path)?.unwrap_or_else(Lockfile::empty);

    // Resolve the new skill BEFORE mutating the manifest.
    // For a re-add where the lock already has a matching entry, reuse it to
    // skip the network resolve; otherwise resolve fresh.
    let new = if skip_manifest_edit {
        // Re-add: the manifest entry already exists with a matching spec.
        // If the lock also has a matching entry, skip the network resolve
        // and reconverge from the existing lock data.
        if let Some(locked) = prev_lock.get(&final_name) {
            if resolve::identity_matches(&parsed.source, &ws.scope_dir, &locked.source)? {
                ui::note!("'{final_name}' lock entry is current; reconverging.");
                locked.clone()
            } else {
                // Lock entry exists but no longer matches — re-resolve.
                resolve::resolve_single_skill(
                    &ws.scope_dir,
                    &ws.scope,
                    &final_name,
                    &parsed.source,
                    &cache,
                    false,
                )?
            }
        } else {
            // Lock missing this entry (partial-failure recovery): resolve.
            resolve::resolve_single_skill(
                &ws.scope_dir,
                &ws.scope,
                &final_name,
                &parsed.source,
                &cache,
                false,
            )?
        }
    } else {
        // Fresh add: always resolve to validate the source.
        resolve::resolve_single_skill(
            &ws.scope_dir,
            &ws.scope,
            &final_name,
            &parsed.source,
            &cache,
            false,
        )?
    };

    // Now we know the source is valid — write the manifest.
    if !skip_manifest_edit {
        let mut doc = manifest::load_doc(&ws.manifest_path)?;
        let entry = NewEntry {
            name: final_name.clone(),
            source: parsed.source.clone(),
            agents: agents_to_write,
        };
        manifest::add_skill(&mut doc, &entry)?;
        manifest::save_doc(&ws.manifest_path, &doc)?;
        ui::ok!(
            "Added '{final_name}' to {}",
            config::display_path(&ws.manifest_path)
        );
    }

    // Merge the resolved skill into the lock.
    let locked_summary = new.source.summary();
    let mut deploy_lock = prev_lock.clone();
    deploy_lock.skills.retain(|s| s.name != final_name);
    deploy_lock.skills.push(new);
    deploy_lock.sort();
    lockfile::write(&ws.lock_path, &deploy_lock)?;
    ui::ok!("Locked '{final_name}' → {locked_summary}");

    if args.no_sync {
        ui::ok!("Updated lock (not deployed; run `skm sync` to install).");
        return Ok(ExitCode::Success);
    }

    // ---- deploy (skip lock_phase — already resolved) ----
    let manifest = manifest::load(&ws.manifest_path)?;
    // Carry over managed extras (skills in lock but not in manifest) so prune
    // can identify ownership.
    let manifest_names: Vec<&str> = manifest.skills.iter().map(|s| s.name.as_str()).collect();
    super::carry_over_managed_extras(&mut deploy_lock, &prev_lock, &manifest_names);

    let deploy_opts = DeployOptions {
        frozen: false,
        locked: false,
        dry_run: false,
        offline: false,
        prune: false, // adding should not prune unrelated extras
        yes: false,
        offline_deferred: Vec::new(), // add never runs an offline dry-run
    };

    let outcome = deploy::deploy(
        &manifest,
        &ws.scope_dir,
        &ws.scope,
        &mut deploy_lock,
        &prev_lock, // ownership snapshot is the pre-add lock
        &cache,
        &deploy_opts,
    )?;

    // Write back if ghost cleanup modified the lock.
    if !lockfile::content_eq(&prev_lock, &deploy_lock) {
        lockfile::write(&ws.lock_path, &deploy_lock)?;
    }

    // `add` is a single-skill action, so report the concrete landing agent +
    // on-disk path per row rather than a bare count (sync keeps the bulk count
    // form for its many-skill case).
    let plan = &outcome.plan;
    let landed = plan.install.iter().chain(plan.repair.iter());
    let mut reported = false;
    for (name, aid) in landed {
        reported = true;
        let verb = if plan.install.iter().any(|(n, a)| n == name && a == aid) {
            "Installed"
        } else {
            "Repaired"
        };
        match aid.skills_root(&ws.scope).ok() {
            Some(root) => ui::ok!(
                "{verb} '{name}' → {aid} ({})",
                config::display_path(&root.join(name))
            ),
            None => ui::ok!("{verb} '{name}' → {aid}"),
        }
    }
    super::report_ghost(plan);
    if !reported && plan.ghost.is_empty() {
        ui::say!("Already up to date.");
    }
    Ok(outcome.exit)
}
