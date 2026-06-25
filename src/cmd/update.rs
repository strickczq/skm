//! `skm update`: re-resolve mutable refs (upgrade) then deploy. A one-shot
//! convenience equal to `skm lock --upgrade [NAME…]` followed by `skm sync`.
//! All the real work lives in [`super::sync::execute`]; `update` only flips the
//! lock phase's [`Upgrade`] policy on the way in.

use crate::error::{ExitCode, Result, SkmError};
use crate::model::config;
use crate::model::manifest;
use crate::pipeline::resolve::Upgrade;

use super::sync::{self, SyncArgs};

pub struct UpdateArgs {
    pub global: bool,
    /// Skills to upgrade. Empty → upgrade all.
    pub names: Vec<String>,
    pub dry_run: bool,
    pub offline: bool,
    pub no_prune: bool,
    pub yes: bool,
}

pub fn run(args: UpdateArgs) -> Result<ExitCode> {
    let ws = config::resolve_workspace(args.global)?;
    let _guard = super::lock_exclusive(&ws)?;
    super::gc_scope(&ws.scope)?;

    // Bare `update` upgrades everything; named args narrow it. (`lock --upgrade`
    // uses an optional flag, so its no-args form means "all" too.)
    let upgrade = if args.names.is_empty() {
        Upgrade::All
    } else {
        Upgrade::Named(args.names)
    };

    // Validate named upgrades exist, mirroring `skm lock`, so a typo fails fast
    // instead of silently upgrading nothing.
    if let Upgrade::Named(names) = &upgrade {
        let manifest = manifest::load(&ws.manifest_path)?;
        for n in names {
            if !manifest.skills.iter().any(|s| &s.name == n) {
                return Err(SkmError::general(format!(
                    "error: cannot upgrade '{n}': not declared in skm.toml"
                )));
            }
        }
    }

    // update is a normal (non-frozen) sync with the upgrade policy switched on:
    // prune defaults on, network allowed unless --offline.
    let sync_args = SyncArgs {
        global: args.global,
        frozen: false,
        locked: false,
        dry_run: args.dry_run,
        offline: args.offline,
        prune: false,
        no_prune: args.no_prune,
        yes: args.yes,
        upgrade,
    };
    sync::execute(&ws, &sync_args)
}
