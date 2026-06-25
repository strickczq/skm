//! `skm cache <dir|clean>`. Operates on the global cache, independent of
//! scope.

use crate::error::{ExitCode, Result, SkmError};
use crate::model::config;
use crate::sys::cache::Cache;
use crate::ui;

pub fn dir() -> Result<ExitCode> {
    let cache = Cache::open(config::cache_dir()?)?;
    ui::say!("{}", cache.root.display());
    Ok(ExitCode::Success)
}

pub fn clean() -> Result<ExitCode> {
    let cache = Cache::open(config::cache_dir()?)?;
    // Exclusive cache lock, fail fast if a sync is materializing.
    let _guard = cache
        .lock_exclusive_nonblocking()
        .map_err(|_| SkmError::io("skm sync is in progress, try again later."))?;
    cache.clean()?;
    ui::ok!("Cleaned cache.");
    Ok(ExitCode::Success)
}
