//! Branch allocation, existing-branch workspaces and default-branch refreshes
//! against workspace records. Adoption is explicit.
use std::path::Path;

use anyhow::{Context, Result, ensure};

use super::Manager;
use crate::{
    git::{
        self,
        default_branch::DefaultBranchLookup,
        existing_branch::{Branch, ensure_not_checked_out},
        fetch,
        repo::{allocate_branch, clean_branch, upstream},
        run_isolated as git_run, worktrunk,
    },
    model::{OpenedWorkspace, PulledBranch, Repository, SyncedRepository},
    state::WorkspaceState,
};

#[derive(Debug, Clone, Copy)]
pub(super) enum UpstreamPolicy {
    Required,
    /// A repository without remotes may use the local branch unchanged.
    AllowLocalOnly,
}

impl Manager {
    /// The requested branch name, or the nearest free `-2`, `-3`, ... variant.
    /// Called while holding the repository's Git gate, through worktree creation.
    pub(super) async fn available_branch(&self, repo: &Repository, name: &str) -> Result<String> {
        let mut taken = git::branch::names_in_use(&repo.path).await?;
        // Failed/preparing workspaces still own their recorded branch name.
        taken.extend(
            self.list_workspaces()
                .await?
                .into_iter()
                .filter(|workspace| workspace.repository_id == repo.id)
                .map(|workspace| workspace.branch),
        );
        Ok(allocate_branch(name, &taken))
    }

    /// Fetch the default branch's remote, updating its remote-tracking branches,
    /// then fast-forward the local default branch as workspace creation does.
    pub async fn sync_repository(
        &self,
        selector: &crate::forge::repository::Selector,
    ) -> Result<SyncedRepository> {
        let repo = self.repository(selector).await?;
        let _guard = self.lock_repository_git(&repo.id).await;
        let default =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Discover).await?;
        let remote = upstream(&repo, &git::local_ref(&default))
            .await?
            .map(|(remote, _)| remote);
        if let Some(remote) = &remote {
            fetch::remote(&repo.path, remote)
                .await
                .with_context(|| format!("could not fetch {remote}"))?;
        }
        let default_branch = self
            .refresh_branch(&repo, &default, UpstreamPolicy::AllowLocalOnly)
            .await
            .with_context(|| format!("could not refresh {default}"))?;
        Ok(SyncedRepository {
            repository_id: repo.id,
            remote,
            default_branch,
        })
    }

    /// Match a canonical checkout path against recorded workspace locations.
    pub(super) async fn managed_workspace_at(&self, path: &Path) -> Result<Option<String>> {
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|w| std::fs::canonicalize(&w.path).is_ok_and(|managed| managed == path))
            .map(|w| w.name))
    }

    /// Fast-forward a local branch from its upstream. The caller holds the
    /// repository's Git gate through any subsequent creation.
    pub(super) async fn refresh_branch(
        &self,
        repo: &Repository,
        branch: &str,
        policy: UpstreamPolicy,
    ) -> Result<PulledBranch> {
        let local_ref = git::local_ref(branch);
        let previous_commit = git::verify_revision(&repo.path, &local_ref, git_run)
            .await
            .with_context(|| format!("repository has no local {branch} branch"))?
            .trim()
            .to_owned();
        let upstream = upstream(repo, &local_ref).await?;
        if matches!(policy, UpstreamPolicy::AllowLocalOnly)
            && upstream.is_none()
            && git::remotes(&repo.path, git_run).await?.trim().is_empty()
        {
            return Ok(PulledBranch::unchanged(
                repo,
                branch,
                previous_commit,
                format!(
                    "{branch} has no upstream and the repository has no remotes; nothing to refresh"
                ),
            ));
        }
        let (remote, reference) = upstream.with_context(|| {
            format!(
                "{branch} has no branch upstream; configure it with git branch --set-upstream-to=<remote>/{branch} {branch}"
            )
        })?;
        let (remote, reference) = (remote.as_str(), reference.as_str());
        let checkout = git::checkout_of(&repo.path, branch, git::run)
            .await?
            .map(|tree| tree.path);
        if let Some(checkout) = &checkout {
            let path = std::fs::canonicalize(checkout)?;
            ensure!(
                self.managed_workspace_at(&path).await?.is_none(),
                "{branch} is checked out in a managed workspace; switch that workspace back to its own branch first"
            );
            clean_branch(checkout, branch).await?;
        }

        let commit = fetch::with_temporary_ref(&repo.path, "pull", git_run, async |fetched| {
            let commit =
                fetch::fetch_commit(&repo.path, remote, reference, fetched, git_run).await?;
            let commit = commit.as_str();
            ensure!(
                git::revision(&repo.path, &local_ref, git_run).await?.trim() == previous_commit,
                "{branch} changed during fetch; retry"
            );
            // Like pull --ff-only, an already-ahead default branch stays untouched.
            if git::is_ancestor(&repo.path, commit, &previous_commit, git::isolated_command).await?
            {
                return Ok(previous_commit.clone());
            }
            ensure!(
                git::is_ancestor(&repo.path, &previous_commit, commit, git::isolated_command)
                    .await?,
                "{branch} and its upstream have diverged; resolve this manually before refreshing"
            );
            if let Some(checkout) = &checkout {
                clean_branch(checkout, branch).await?;
                git::branch::fast_forward(checkout, commit).await?;
            } else {
                git::fast_forward_local(&repo.path, commit, &local_ref).await?;
            }
            Ok(commit.to_owned())
        })
        .await?;
        Ok(PulledBranch {
            branch: branch.into(),
            repository_id: repo.id.clone(),
            updated: commit != previous_commit,
            previous_commit,
            commit,
            skipped: None,
        })
    }

    /// Query advertised remote heads, including branches never fetched locally.
    pub async fn branches(
        &self,
        repository: impl Into<crate::forge::repository::Selector>,
    ) -> Result<Vec<Branch>> {
        let repo = self.repository(repository).await?;
        self.ensure_repository_available(&repo.id).await?;
        git::existing_branch::list(&repo.path).await
    }

    pub async fn open_branch(
        &self,
        repository: impl Into<crate::forge::repository::Selector>,
        selector: &str,
        git_profile: Option<&str>,
        path: Option<std::path::PathBuf>,
        base: Option<String>,
    ) -> Result<OpenedWorkspace> {
        let (repo, _guard) = self.lock_repository(repository).await?;
        let local = git::strip_local(selector).unwrap_or(selector);
        let local_ref = git::local_ref(local);
        let branch = if git::strip_remote(selector).is_none()
            && git::ref_exists(&repo.path, &local_ref, git::isolated_command).await?
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
                git::ref_exists(&repo.path, &git::local_ref(name), git::isolated_command).await?;
            if local_exists {
                let upstream = git::existing_branch::upstream_ref(&repo.path, name).await?;
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
        ensure_not_checked_out(&repo.path, name).await?;
        let workspace = self
            .create_branch_workspace(
                &repo,
                name.clone(),
                name.clone(),
                super::WorkspaceSource::Existing(branch, base),
                git_profile,
                path,
            )
            .await?;
        Ok(OpenedWorkspace {
            workspace,
            reused: false,
        })
    }

    pub(super) async fn materialize_branch(
        &self,
        repo: &Repository,
        branch: &Branch,
    ) -> Result<()> {
        if let Some(remote) = &branch.remote {
            let tracking = fetch::materialize_branch(&repo.path, remote, &branch.name).await?;
            let local = git::local_ref(&branch.name);
            if git::ref_exists(&repo.path, &local, git::isolated_command).await? {
                self.refresh_branch(repo, &branch.name, UpstreamPolicy::Required)
                    .await?;
            } else {
                git::existing_branch::track(&repo.path, &branch.name, &tracking).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
