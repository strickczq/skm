//! The two-phase pipeline: manifest → resolve (lock phase) → deploy (sync
//! phase). `resolve` decides *what* to install and writes skm.lock without
//! touching disk; `deploy` is lock-driven and converges the skills dirs.

pub mod deploy;
pub mod resolve;
