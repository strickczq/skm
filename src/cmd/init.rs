//! `skm init`.

use crate::error::{ExitCode, Result, SkmError};
use crate::model::agent;
use crate::model::config;
use crate::model::manifest;
use crate::ui;

pub fn run(global: bool, force: bool) -> Result<ExitCode> {
    let ws = config::resolve_workspace_for_create(global)?;
    if ws.manifest_path.is_file() && !force {
        return Err(SkmError::general(format!(
            "error: {} already exists; use --force to overwrite",
            config::display_path(&ws.manifest_path)
        )));
    }
    std::fs::create_dir_all(&ws.scope_dir).map_err(|e| {
        SkmError::io(format!(
            "cannot create '{}': {e}",
            config::display_path(&ws.scope_dir)
        ))
    })?;
    manifest::atomic_write_str(&ws.manifest_path, &manifest::template())?;

    ui::ok!("Created {}", config::display_path(&ws.manifest_path));
    ui::say!(
        "Set [defaults].agents ({}) before adding skills, or pass --agent per add.",
        agent::valid_agents()
    );
    ui::say!(
        "Consider adding the skills folder to .gitignore, or committing it to version control."
    );
    ui::say!("Run 'skm sync' to install declared skills and generate skm.lock.");
    Ok(ExitCode::Success)
}
