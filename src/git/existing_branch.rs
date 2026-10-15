//! Existing branches a workspace can open, and the checkout rule for them.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::git;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub remote: Option<String>,
}

impl Branch {
    pub fn selector(&self) -> String {
        match &self.remote {
            Some(remote) => git::remote_ref(remote, &self.name),
            None => git::local_ref(&self.name),
        }
    }
}

/// A worktree for `name` can be created only where no checkout already has it.
pub async fn ensure_not_checked_out(repo: &std::path::Path, name: &str) -> Result<()> {
    if let Some(tree) = git::checkout_of(repo, name, git::run).await? {
        anyhow::bail!(
            "branch {name} is already checked out at {}; Shoal cannot create another worktree for it; use shoal adopt for an unmanaged linked worktree",
            tree.path.display()
        );
    }
    Ok(())
}
