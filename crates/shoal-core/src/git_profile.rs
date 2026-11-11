//! Writing a named Git identity into a worktree's own Git config, so the
//! shared repository config and the other worktrees keep theirs.
use std::path::Path;

use anyhow::Result;

pub(crate) use crate::config::git_profiles::{Git, Profile};
use crate::git;

/// Write `profile` into the worktree's own config. The first profile in a
/// repository enables Git's `worktreeConfig` extension in the shared config;
/// a shared `core.worktree` or `core.bare = true` would then break the main
/// checkout unless migrated to its config.worktree, so those are refused.
pub async fn apply(worktree: &Path, profile: &Profile) -> Result<()> {
    let settings = profile.settings()?;
    git::config::apply_worktree_settings(worktree, &settings).await
}

#[cfg(test)]
pub(crate) mod tests;
