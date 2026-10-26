//! Daemon diagnostics, separate from workspace recovery mutations. They
//! change no work; asking forge hosts again records their answers.
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{
        environment::{ROOT_DIR_CHECK, checkout, checkout_check_name, dependencies, root_dir},
        workspace::Manager,
    },
    git,
    state::states,
    tools::Tool,
};

states!(CheckStatus {
    Ok => "ok",
    Warning => "warning",
    Error => "error",
    Skipped => "skipped",
});

#[derive(Debug, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

impl Check {
    pub fn new(name: impl Into<String>, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status,
            message: message.into(),
        }
    }
}

pub fn worktrees_check_name(repository: &str) -> String {
    format!("worktrees:{repository}")
}

/// Repository names belong to the daemon and are unknown when it is unavailable.
pub fn unavailable_checks() -> Vec<Check> {
    Tool::dependencies()
        .map(|(tool, _)| tool.check_name())
        .chain([
            "cleanup".to_owned(),
            ROOT_DIR_CHECK.to_owned(),
            checkout_check_name("*"),
            crate::daemon::forge_hosts::forge_check_name("*"),
            worktrees_check_name("*"),
            "workspaces".to_owned(),
        ])
        .map(|name| {
            Check::new(
                name,
                CheckStatus::Skipped,
                "Not checked: a reachable, matching daemon is required",
            )
        })
        .collect()
}

/// One warning per problem, or one passing check when there are none.
fn worktree_report(name: String, root: &Path, problems: Vec<String>) -> Vec<Check> {
    if problems.is_empty() {
        return vec![Check::new(
            name,
            CheckStatus::Ok,
            format!("No untracked Git worktrees under {}", root.display()),
        )];
    }
    problems
        .into_iter()
        .map(|problem| Check::new(&name, CheckStatus::Warning, problem))
        .collect()
}

impl Manager {
    pub async fn diagnose(&self) -> Result<Vec<Check>> {
        let mut checks = dependencies(std::env::var_os("PATH").as_deref());
        checks.push(self.cleanup_check());
        checks.push(root_dir(self.config().root_dir(&self.paths)).await);
        checks.extend(self.forge_host_checks().await?);
        for repo in self.repositories().await? {
            checks.push(checkout(&repo).await);
            checks.extend(self.worktree_checks(&repo).await?);
        }
        Ok(checks)
    }

    /// Git worktrees under the repository's workspace directory that Shoal
    /// does not own; none for a repository without one.
    async fn worktree_checks(&self, repo: &crate::model::Repository) -> Result<Vec<Check>> {
        let Some(root) = &repo.workspaces_dir else {
            return Ok(Vec::new());
        };
        let name = worktrees_check_name(crate::repository::name(repo));
        // Take the same gate as creation/removal before reading ownership.
        let _guard = self.lock_repository_git(&repo.id).await;
        let owned = self.owned_worktrees(repo, root).await?;
        Ok(match git::worktrees(&repo.path, git::run).await {
            Ok(trees) => worktree_report(name, root, owned.problems(&repo.path, root, &trees)),
            Err(error) => vec![Check::new(
                name,
                CheckStatus::Error,
                format!(
                    "Could not check worktrees under {}: {error:#}",
                    root.display()
                ),
            )],
        })
    }
}

#[cfg(test)]
mod tests;
