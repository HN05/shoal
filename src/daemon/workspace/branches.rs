//! Branch allocation and default-branch refreshes against workspace records.
use std::path::Path;

use anyhow::{Context, Result, ensure};

use super::Manager;
use crate::{
    git::{
        self,
        default_branch::DefaultBranchLookup,
        fetch,
        repo::{allocate_branch, clean_branch, upstream},
        run_isolated as git_run,
    },
    model::{PulledBranch, Repository, SyncedRepository},
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum UpstreamPolicy {
    Required,
    /// A repository without remotes may use the local branch unchanged.
    AllowLocalOnly,
}

impl Manager {
    /// The requested branch name, or the nearest free `-2`, `-3`, ... variant.
    /// Called while holding the repository's Git gate, through worktree creation.
    pub(super) async fn available_branch(&self, repo: &Repository, name: &str) -> Result<String> {
        let refs = git::run(
            &repo.path,
            &[
                "for-each-ref",
                "--format=%(refname)",
                git::LOCAL_REFS,
                git::REMOTE_REFS,
            ],
        )
        .await?;
        let mut taken: Vec<String> = refs
            .lines()
            .filter_map(|reference| {
                git::strip_local(reference).or_else(|| {
                    git::strip_remote(reference)?
                        .split_once('/')
                        .map(|(_, branch)| branch)
                })
            })
            .map(str::to_owned)
            .collect();
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
            git_run(
                &repo.path,
                &[git::FETCH_SAFE_ARGS, &["--", remote]].concat(),
            )
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
    pub(crate) async fn refresh_branch(
        &self,
        repo: &Repository,
        branch: &str,
        policy: UpstreamPolicy,
    ) -> Result<PulledBranch> {
        let local_ref = git::local_ref(branch);
        let previous_commit = git_run(&repo.path, &["rev-parse", "--verify", &local_ref])
            .await
            .with_context(|| format!("repository has no local {branch} branch"))?
            .trim()
            .to_owned();
        let upstream = upstream(repo, &local_ref).await?;
        if matches!(policy, UpstreamPolicy::AllowLocalOnly)
            && upstream.is_none()
            && git_run(&repo.path, &["remote"]).await?.trim().is_empty()
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
                git_run(&repo.path, &["rev-parse", &local_ref])
                    .await?
                    .trim()
                    == previous_commit,
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
                git::run_without_submodules(
                    checkout,
                    &[
                        "merge",
                        "--ff-only",
                        "--no-edit",
                        "--no-stat",
                        "--no-overwrite-ignore",
                        commit,
                    ],
                )
                .await?;
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
}

#[cfg(test)]
mod tests;
