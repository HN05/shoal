//! The daemon a state directory's lock names as its owner.
use std::{fs, fs::File, os::unix::fs::FileExt};

use anyhow::{Context, Result};

use crate::{
    paths::Paths,
    process::identity::{self, Identity},
};

/// Name this process in the lock, so clients can tell a daemon that holds the
/// state directory without answering from one that is not running.
pub(crate) fn record(lock: &File) -> Result<()> {
    let owner = identity::capture(std::process::id())?.context("this process has no identity")?;
    lock.set_len(0)?;
    lock.write_all_at(&serde_json::to_vec(&owner)?, 0)?;
    Ok(())
}

/// The live daemon that owns the state directory, as its lock names it.
pub(crate) fn owner(paths: &Paths) -> Option<Identity> {
    let recorded = fs::read(paths.daemon_lock()).ok()?;
    let owner = serde_json::from_slice(&recorded).ok()?;
    identity::alive(&owner).ok()?.then_some(owner)
}

#[cfg(test)]
mod tests;
