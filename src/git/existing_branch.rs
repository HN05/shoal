//! Existing-branch selection creates or reopens worktrees; adoption is explicit.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    daemon::workspace::Manager,
    git::{self, repo::UpstreamPolicy, worktrunk},
    model::{Repository, Workspace},
    state::WorkspaceState,
};

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
            &[
                "for-each-ref",
                "--format=%(refname:strip=2)",
                git::LOCAL_REFS,
            ],
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
                    name: git::strip_local(reference)?.into(),
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
        base: Option<String>,
    ) -> Result<OpenedWorkspace> {
        let (repo, _guard) = self.lock_repository(repository).await?;
        let local = git::strip_local(selector).unwrap_or(selector);
        let local_ref = git::local_ref(local);
        let branch = if git::strip_remote(selector).is_none()
            && git::ref_exists(&repo.path, &local_ref, git::isolated_command)
                .await
                .unwrap_or(false)
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
            !worktrunk::is_reserved_branch_name(name),
            "branch name is reserved by Worktrunk: {name}"
        );
        git::check_branch_name(Some(&repo.path), name).await?;
        if let Some(remote) = &branch.remote {
            let local_exists =
                git::ref_exists(&repo.path, &git::local_ref(name), git::isolated_command)
                    .await
                    .unwrap_or(false);
            if local_exists {
                let upstream = git::run_isolated(
                    &repo.path,
                    &[
                        "for-each-ref",
                        "--format=%(upstream)",
                        &git::local_ref(name),
                    ],
                )
                .await?;
                ensure!(
                    upstream.trim() == git::remote_ref(remote, name),
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
            ensure!(
                base.is_none(),
                "--base applies only to new worktrees; workspace {} already exists",
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
            let actual = git::head_branch(&workspace.path, false, git::run_isolated).await?;
            ensure!(
                actual.as_deref() == Some(name),
                "workspace {} is no longer on its recorded branch",
                workspace.name
            );
            return Ok(OpenedWorkspace {
                workspace,
                reused: true,
            });
        }
        if let Some(tree) = git::checkout_of(&repo.path, name, git::run).await? {
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
                crate::daemon::workspace::WorkspaceSource::Existing(branch, base),
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
            let tracking = git::remote_ref(remote, &branch.name);
            git::run_isolated(
                &repo.path,
                &[
                    &["-c", "fetch.prune=false"][..],
                    git::FETCH_SAFE_ARGS,
                    &[
                        "--refmap=",
                        "--",
                        remote,
                        &format!("+{}:{tracking}", git::local_ref(&branch.name)),
                    ],
                ]
                .concat(),
            )
            .await?;
            let local = git::local_ref(&branch.name);
            if git::ref_exists(&repo.path, &local, git::isolated_command)
                .await
                .unwrap_or(false)
            {
                self.refresh_branch(repo, &branch.name, UpstreamPolicy::Required)
                    .await?;
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
