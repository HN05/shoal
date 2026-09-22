//! Existing-branch selection creates or reopens worktrees; adoption is explicit.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    git,
    model::{Repository, Workspace},
    state::WorkspaceState,
    workspace::Manager,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub remote: Option<String>,
}

impl Branch {
    pub fn selector(&self) -> String {
        match &self.remote {
            Some(remote) => format!("refs/remotes/{remote}/{}", self.name),
            None => format!("refs/heads/{}", self.name),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenedWorkspace {
    pub workspace: Workspace,
    pub reused: bool,
}

impl Manager {
    /// Query advertised remote heads, including branches never fetched locally.
    pub async fn branches(&self, repository: &str) -> Result<Vec<Branch>> {
        let repo = self.repository(repository).await?;
        self.ensure_repository_available(&repo.id).await?;
        let mut branches: Vec<_> = git::run_isolated(
            &repo.path,
            &["for-each-ref", "--format=%(refname:strip=2)", "refs/heads/"],
        )
        .await?
        .lines()
        .map(|name| Branch {
            name: name.into(),
            remote: None,
        })
        .collect();
        for remote in git::run_isolated(&repo.path, &["remote"]).await?.lines() {
            let heads = git::run_isolated(&repo.path, &["ls-remote", "--heads", "--", remote])
                .await.with_context(|| format!("could not list branches from {remote}; use --existing <local-branch> to work offline"))?;
            branches.extend(heads.lines().filter_map(|line| {
                let (_, reference) = line.split_once('\t')?;
                Some(Branch {
                    name: reference.strip_prefix("refs/heads/")?.into(),
                    remote: Some(remote.into()),
                })
            }));
        }
        Ok(branches)
    }

    pub async fn open_branch(
        &self,
        repository: &str,
        selector: &str,
        git_profile: Option<&str>,
        path: Option<std::path::PathBuf>,
    ) -> Result<OpenedWorkspace> {
        let repo = self.repository(repository).await?;
        let gate = self.git_gate(&repo.id).await;
        let _guard = gate.lock().await;
        self.repository(&repo.id).await?;
        self.ensure_repository_available(&repo.id).await?;
        let local = selector.strip_prefix("refs/heads/").unwrap_or(selector);
        let local_ref = format!("refs/heads/{local}");
        let branch = if !selector.starts_with("refs/remotes/")
            && git::run_isolated(&repo.path, &["show-ref", "--verify", "--", &local_ref])
                .await
                .is_ok()
        {
            Branch {
                name: local.into(),
                remote: None,
            }
        } else {
            let matches: Vec<_> = self
                .branches(&repo.id)
                .await?
                .into_iter()
                .filter(|branch| {
                    branch.selector() == selector
                        || branch.remote.as_ref().is_some_and(|remote| {
                            format!("{remote}/{}", branch.name) == selector
                                || branch.name == selector
                        })
                })
                .collect();
            ensure!(
                matches.len() == 1,
                "branch {selector} is missing or ambiguous; select a specific local or remote branch"
            );
            matches.into_iter().next().unwrap()
        };
        let name = &branch.name;
        ensure!(
            name != "HEAD"
                && name != "@"
                && !(name.len() >= 40 && name.chars().all(|c| c.is_ascii_hexdigit())),
            "branch name is reserved by Worktrunk: {name}"
        );
        let validated =
            git::run_isolated(&repo.path, &["check-ref-format", "--branch", name]).await?;
        ensure!(
            validated == format!("{name}\n"),
            "use a literal Git branch name"
        );
        if let Some(remote) = &branch.remote {
            let local_exists = git::run_isolated(
                &repo.path,
                &["show-ref", "--verify", "--", &format!("refs/heads/{name}")],
            )
            .await
            .is_ok();
            if local_exists {
                let upstream = git::run_isolated(
                    &repo.path,
                    &[
                        "for-each-ref",
                        "--format=%(upstream)",
                        &format!("refs/heads/{name}"),
                    ],
                )
                .await?;
                ensure!(
                    upstream.trim() == format!("refs/remotes/{remote}/{name}"),
                    "local branch {name} already exists and does not track {remote}/{name}; select the local branch explicitly"
                );
            }
        }
        if let Some(workspace) = self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|w| w.repository_id == repo.id && w.branch == *name)
        {
            ensure!(
                workspace.state == WorkspaceState::Ready,
                "workspace {} is {}; inspect or set it up before reopening",
                workspace.name,
                workspace.state
            );
            ensure!(
                git_profile.is_none(),
                "--git-profile applies only to new worktrees; workspace {} already exists",
                workspace.name
            );
            if let Some(path) = &path {
                ensure!(
                    std::fs::canonicalize(path).is_ok_and(|p| p == workspace.path),
                    "workspace already exists at {}; --path cannot relocate it",
                    workspace.path.display()
                );
            }
            self.verify_worktree(&workspace).await?;
            let actual = git::run_isolated(&workspace.path, &["symbolic-ref", "HEAD"]).await?;
            ensure!(
                actual.trim() == format!("refs/heads/{name}"),
                "workspace {} is no longer on its recorded branch",
                workspace.name
            );
            return Ok(OpenedWorkspace {
                workspace,
                reused: true,
            });
        }
        if let Some(tree) = git::worktrees(&repo.path)
            .await?
            .into_iter()
            .find(|tree| tree.is_branch(name))
        {
            anyhow::bail!(
                "branch {name} is already checked out at {}; Shoal cannot create another worktree for it; use shoal adopt for an unmanaged linked worktree",
                tree.path.display()
            );
        }
        let workspace = self
            .create_branch_workspace(
                &repo,
                name.clone(),
                name.clone(),
                crate::workspace::WorkspaceSource::Existing(branch),
                git_profile,
                path,
            )
            .await?;
        Ok(OpenedWorkspace {
            workspace,
            reused: false,
        })
    }

    pub(crate) async fn materialize_branch(
        &self,
        repo: &Repository,
        branch: &Branch,
    ) -> Result<()> {
        if let Some(remote) = &branch.remote {
            let tracking = format!("refs/remotes/{remote}/{}", branch.name);
            git::run_isolated(
                &repo.path,
                &[
                    "-c",
                    "fetch.prune=false",
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--refmap=",
                    "--",
                    remote,
                    &format!("+refs/heads/{}:{tracking}", branch.name),
                ],
            )
            .await?;
            let local = format!("refs/heads/{}", branch.name);
            if git::run_isolated(&repo.path, &["show-ref", "--verify", "--", &local])
                .await
                .is_ok()
            {
                self.refresh_branch(repo, &branch.name, false).await?;
            } else {
                git::run_isolated(
                    &repo.path,
                    &["branch", "--track", "--", &branch.name, &tracking],
                )
                .await?;
            }
        }
        Ok(())
    }
}
