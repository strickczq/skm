//! `skm status`. Read-only five-state table per (skill, agent).

use std::collections::BTreeSet;

use crate::error::{ExitCode, Result};
use crate::model::agent::{Agent, parse_agents};
use crate::model::config;
use crate::model::lockfile;
use crate::model::manifest;
use crate::pipeline::deploy::list_root_dirs;
use crate::sys::hash;
use crate::ui;

pub fn run(global: bool) -> Result<ExitCode> {
    let ws = config::resolve_workspace(global)?;
    let _guard = super::lock_shared(&ws)?;

    let manifest = manifest::load(&ws.manifest_path)?;
    let lock_opt = lockfile::load(&ws.lock_path)?;

    // Sorted skills (byte order) for stable display.
    let mut skills: Vec<&manifest::ManifestSkill> = manifest.skills.iter().collect();
    skills.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

    let Some(lock) = lock_opt else {
        for skill in &skills {
            let ids = skill.effective_agents(&manifest.default_agents)?;
            let mut agents = parse_agents(&ids)?;
            agents.sort_by(|a, b| a.id().as_bytes().cmp(b.id().as_bytes()));
            ui::heading!("{}", skill.name);
            ui::say!("  source: {}", skill.source.summary());
            for a in agents {
                ui::say!(
                    "  {:<12} {}",
                    a.id(),
                    ui::paint(ui::Tone::Attn, "unknown (lock missing)")
                );
            }
        }
        ui::say!("");
        ui::heading!("Notice");
        ui::say!("lock missing — ownership unavailable");
        ui::say!("Run `skm sync` to regenerate lock and restore ownership tracking.");
        return Ok(ExitCode::General);
    };

    let mut worst = ExitCode::Success;
    let bump = |c: ExitCode, worst: &mut ExitCode| {
        if c.severity_rank() > worst.severity_rank() {
            *worst = c;
        }
    };

    // Track declared (name, agent) for extra detection.
    let mut declared: BTreeSet<(String, Agent)> = BTreeSet::new();
    // State kinds actually shown, so the footer legend explains only what's
    // on screen ("installed" needs no gloss and is never recorded).
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();

    for skill in &skills {
        let ids = skill.effective_agents(&manifest.default_agents)?;
        let mut agents = parse_agents(&ids)?;
        agents.sort_by(|a, b| a.id().as_bytes().cmp(b.id().as_bytes()));
        ui::heading!("{}", skill.name);
        // What's pinned: prefer the lock (resolved commit/digest), falling back
        // to the manifest's declared source when not yet locked.
        let src = match lock.get(&skill.name) {
            Some(e) => e.source.summary(),
            None => skill.source.summary(),
        };
        ui::say!("  source: {src}");
        for a in agents {
            declared.insert((skill.name.clone(), a));
            let root = a.skills_root(&ws.scope)?;
            let final_path = root.join(&skill.name);
            let (state, tone) = if !final_path.exists() {
                bump(ExitCode::General, &mut worst);
                seen.insert("missing");
                ("missing".to_string(), ui::Tone::Attn)
            } else {
                match lock.get(&skill.name) {
                    None => {
                        bump(ExitCode::General, &mut worst);
                        seen.insert("unlocked");
                        ("unlocked".to_string(), ui::Tone::Attn)
                    }
                    Some(entry) => {
                        match hash::hash_tree(&final_path, entry.source.source_type().exec_policy())
                        {
                            Ok(h) if h == entry.content_sha256 => {
                                ("installed".to_string(), ui::Tone::Good)
                            }
                            Ok(_) => {
                                bump(ExitCode::Drift, &mut worst);
                                seen.insert("drift");
                                ("drift (content changed)".to_string(), ui::Tone::Attn)
                            }
                            Err(_) => {
                                bump(ExitCode::Drift, &mut worst);
                                seen.insert("drift");
                                ("drift (content unreadable)".to_string(), ui::Tone::Attn)
                            }
                        }
                    }
                }
            };
            ui::say!("  {:<12} {}", a.id(), ui::paint(tone, &state));
        }
    }

    // Extra / foreign section.
    let mut extras: Vec<(String, Agent, bool)> = Vec::new();
    for a in Agent::ALL {
        let root = a.skills_root(&ws.scope)?;
        for dir in list_root_dirs(&root) {
            if declared.contains(&(dir.clone(), a)) {
                continue;
            }
            let managed = lock.get(&dir).is_some();
            extras.push((dir, a, managed));
        }
    }
    extras.sort_by(|a, b| {
        a.0.as_bytes()
            .cmp(b.0.as_bytes())
            .then(a.1.id().cmp(b.1.id()))
    });

    if !extras.is_empty() {
        ui::say!("");
        ui::heading!("Extra / foreign");
        let mut last = String::new();
        for (name, a, managed) in &extras {
            if *name != last {
                ui::heading!("{name}");
                last = name.clone();
            }
            if *managed {
                seen.insert("extra");
                ui::say!(
                    "  {:<12} {}",
                    a.id(),
                    ui::paint(ui::Tone::Attn, "extra (managed — will be pruned)")
                );
            } else {
                seen.insert("foreign");
                ui::say!("  {:<12} {}", a.id(), ui::paint(ui::Tone::Info, "foreign"));
            }
        }
    }

    print_legend(&seen);
    Ok(worst)
}

/// Print a footer explaining only the non-trivial state kinds that appeared
/// (skipped entirely when everything is `installed`).
fn print_legend(seen: &BTreeSet<&'static str>) {
    if seen.is_empty() {
        return;
    }
    // Order matches the table's severity-ish reading order; the tone mirrors
    // the state word's color in the table above.
    let defs = [
        (
            "missing",
            ui::Tone::Attn,
            "declared but not deployed to this agent",
        ),
        (
            "drift",
            ui::Tone::Attn,
            "on-disk content no longer matches skm.lock",
        ),
        (
            "unlocked",
            ui::Tone::Attn,
            "on disk but absent from skm.lock — run `skm sync`",
        ),
        (
            "extra",
            ui::Tone::Attn,
            "managed by skm but no longer declared",
        ),
        (
            "foreign",
            ui::Tone::Info,
            "present on disk but never managed by skm",
        ),
    ];
    ui::say!("");
    ui::heading!("Legend");
    for (kind, tone, desc) in defs {
        if seen.contains(kind) {
            // Pad first (alignment counts visible width), then paint.
            ui::say!("  {} {desc}", ui::paint(tone, &format!("{kind:<10}")));
        }
    }
}
