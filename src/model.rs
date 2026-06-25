//! Typed models of skm's persistent state: the manifest (skm.toml), the
//! lockfile (skm.lock), scope/workspace configuration, and agents.

pub mod agent;
pub mod config;
pub mod lockfile;
pub mod manifest;
