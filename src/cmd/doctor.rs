//! `skm doctor` — read-only environment diagnostics.

use std::process::Command;

use crate::error::{ExitCode, Result};
use crate::model::agent::Agent;
use crate::model::config::{self, Workspace};
use crate::model::lockfile;
use crate::model::manifest;
use crate::sys::cache::Cache;
use crate::sys::fsutil;
use crate::sys::hash;
use crate::ui;

pub fn run(global: bool) -> Result<ExitCode> {
    let ws = config::resolve_workspace(global)?;
    let _guard = super::lock_shared(&ws)?;
    diagnose(&ws)
}

/// Run diagnostics; returns the most-severe exit code.
fn diagnose(ws: &Workspace) -> Result<ExitCode> {
    let mut worst = ExitCode::Success;
    let bump = |c: ExitCode, worst: &mut ExitCode| {
        if c.severity_rank() > worst.severity_rank() {
            *worst = c;
        }
    };

    // git in PATH.
    let git_ok = Command::new("git").arg("--version").output().is_ok();
    if git_ok {
        ui::report_ok!("git is available");
    } else {
        ui::report_error!("git not found in PATH");
        bump(ExitCode::General, &mut worst);
    }

    // cache readable/writable.
    match Cache::open(config::cache_dir()?) {
        Ok(cache) => {
            let probe = cache.root.join(".doctor-probe");
            match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
                Ok(()) => {
                    ui::report_ok!("cache is writable ({})", config::display_path(&cache.root))
                }
                Err(e) => {
                    ui::report_error!("cache not writable: {e}");
                    bump(ExitCode::Io, &mut worst);
                }
            }
        }
        Err(e) => {
            ui::report_error!("cannot open cache: {}", ui::strip_prefix(&e.message));
            bump(ExitCode::Io, &mut worst);
        }
    }

    // Lock load + version + leftover temp dirs + reference integrity + exec bit.
    let lock = lockfile::load(&ws.lock_path)?;
    let manifest = if ws.manifest_path.is_file() {
        Some(manifest::load(&ws.manifest_path)?)
    } else {
        None
    };

    if let Some(lock) = &lock {
        // generated_by compatibility note (informational, semver rule).
        if let Some(msg) = lockfile::compat_note(&lock.generated_by, &lockfile::generated_by()) {
            ui::report_note!("{}", msg);
        }
    }

    // skills_root permissions: for each existing root, probe-write a file
    // and delete it. Non-existent roots are silently skipped (sync will create
    // them); a write failure on an existing root bumps Io.
    for a in Agent::ALL {
        let root = match a.skills_root(&ws.scope) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !root.is_dir() {
            continue;
        }
        let probe = root.join(format!(".skm-doctor-probe.{}", fsutil::rand_token()));
        match std::fs::write(&probe, b"ok") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                ui::report_ok!("skills_root writable ({})", config::display_path(&root));
            }
            Err(e) => {
                ui::report_error!(
                    "skills_root not writable ({}): {e}",
                    config::display_path(&root)
                );
                bump(ExitCode::Io, &mut worst);
            }
        }
    }

    // Orphan temp dirs across all roots.
    let mut orphans = 0usize;
    for t in Agent::ALL {
        if let Ok(root) = t.skills_root(&ws.scope) {
            orphans += fsutil::count_temp_dirs(&root);
        }
    }
    if orphans > 0 {
        ui::report_warn!("{orphans} leftover temp directories (*.skm-new.* / *.skm-old.*)");
        ui::say!("  → run `skm sync` to clean them up");
        bump(ExitCode::General, &mut worst);
    } else {
        ui::report_ok!("no leftover temp directories");
    }

    // Reference integrity + exec-bit integrity (recompute hash).
    if let (Some(lock), Some(manifest)) = (&lock, &manifest) {
        for skill in &manifest.skills {
            if lock.get(&skill.name).is_none() {
                ui::report_error!("'{}' in manifest but missing from lock", skill.name);
                bump(ExitCode::General, &mut worst);
            }
        }
        let ids: Vec<&str> = manifest.skills.iter().map(|s| s.name.as_str()).collect();
        for entry in &lock.skills {
            for t in Agent::ALL {
                let root = t.skills_root(&ws.scope)?;
                let dir = root.join(&entry.name);
                if !dir.is_dir() {
                    continue;
                }
                if !ids.contains(&entry.name.as_str()) {
                    continue;
                }
                match hash::hash_tree(&dir, entry.source.source_type().exec_policy()) {
                    Ok(h) if h == entry.content_sha256 => {}
                    Ok(_) => {
                        ui::report_error!(
                            "exec-bit / content drift in '{}' ({})",
                            entry.name,
                            t.id()
                        );
                        ui::say!("  → run `skm sync` to repair");
                        bump(ExitCode::Drift, &mut worst);
                    }
                    Err(e) => {
                        ui::report_error!(
                            "cannot hash '{}': {}",
                            config::display_path(&dir),
                            ui::strip_prefix(&e.message)
                        );
                        bump(ExitCode::Io, &mut worst);
                    }
                }
            }
        }
    }

    if worst == ExitCode::Success {
        ui::say!("");
        ui::heading!("healthy");
    }
    Ok(worst)
}
