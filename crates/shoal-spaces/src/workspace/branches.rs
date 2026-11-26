//! Branch allocation, workspaces on existing branches, and default-branch
//! refreshes.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::git::existing_branch::BranchExt;
use shoal_ids::repository_ids::RepositoryId;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use shoal_refs::{BranchName, CommitId};

use crate::{
    fsutil::off_runtime,
    git::{
        self,
        default_branch::DefaultBranchLookup,
        existing_branch::{Branch, ensure_not_checked_out},
        fetch,
        repo::{allocate_branch, clean_branch, upstream},
        run_isolated as git_run,
    },
    model::{OpenedWorkspace, PulledBranch, Repository, SyncedRepository, Workspace},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct NewWorktreeOptions<'a> {
    pub git_profile: Option<&'a str>,
    pub path: Option<&'a Path>,
    pub base: Option<&'a str>,
    pub disposable: bool,
}

pub(super) enum OpenTarget {
    Reopen(Box<Workspace>),
    Create(Branch),
}

/// Allows only a path that names where the workspace already is.
async fn check_reopen_options(
    workspace: &Workspace,
    options: NewWorktreeOptions<'_>,
) -> Result<()> {
    ensure!(
        options.git_profile.is_none(),
        "--git-profile applies only to new worktrees; workspace {} already exists",
        workspace.name
    );
    ensure!(
        options.base.is_none(),
        "--base applies only to new worktrees; workspace {} already exists",
        workspace.name
    );
    if let Some(path) = options.path {
        ensure!(
            off_runtime::canonicalize(path)
                .await
                .is_ok_and(|p| p == workspace.path),
            "workspace already exists at {}; --path cannot relocate it",
            workspace.path.display()
        );
    }
    Ok(())
}

enum Refresh {
    /// Nothing: the branch has no upstream to follow.
    Unchanged(PulledBranch),
    Follow(Following),
}

struct Following {
    branch: BranchName,
    local_ref: String,
    previous_commit: CommitId,
    remote: String,
    reference: String,
    checkout: Option<std::path::PathBuf>,
}

impl Following {
    fn pulled(self, repo: &Repository, commit: CommitId) -> PulledBranch {
        PulledBranch {
            branch: self.branch,
            repository_id: repo.id.clone(),
            updated: commit != self.previous_commit,
            previous_commit: self.previous_commit,
            commit,
            skipped: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum UpstreamPolicy {
    Required,
    /// A repository without remotes may use the local branch unchanged.
    AllowLocalOnly,
}

impl Spaces {
    /// The requested name, or the nearest free `-2`, `-3`, ... variant.
    /// Called with the repository's Git gate held, through worktree
    /// creation.
    pub async fn available_branch(&self, repo: &Repository, name: &str) -> Result<BranchName> {
        Ok(allocate_branch(name, &self.branches_taken(repo).await?))
    }

    /// For a copy of the work on `name`, whether or not `name` exists here.
    pub async fn copy_branch(&self, repo: &Repository, name: &str) -> Result<BranchName> {
        let mut taken = self.branches_taken(repo).await?;
        taken.push(name.into());
        Ok(allocate_branch(name, &taken))
    }

    async fn branches_taken(&self, repo: &Repository) -> Result<Vec<BranchName>> {
        let mut taken = git::branch::names_in_use(&repo.path).await?;
        // Failed and preparing workspaces still own their recorded branch
        // name.
        taken.extend(
            self.list_workspaces()
                .await?
                .into_iter()
                .filter(|workspace| workspace.repository() == &repo.id)
                .filter_map(|workspace| workspace.part.branch().cloned()),
        );
        Ok(taken)
    }

    /// Fetches the local default branch's upstream and the default remote,
    /// so merge checks see what the forge merged. Local branches stay as
    /// they are. The caller holds the repository's fetch gate.
    pub async fn fetch_default_remotes(&self, repo: &Repository) -> Result<BranchName> {
        let default =
            crate::git::default_branch::locate(&repo.path, DefaultBranchLookup::Discover).await?;
        let mut remotes = Vec::new();
        let local = git::local_ref(&default.name);
        if git::ref_exists(&repo.path, &local).await?
            && let Some((remote, _)) = upstream(repo, &local).await?
        {
            remotes.push(remote);
        }
        if let Some(remote) = default.remote
            && !remotes.contains(&remote)
        {
            remotes.push(remote);
        }
        for remote in remotes {
            fetch::remote(&repo.path, &remote)
                .await
                .with_context(|| format!("could not fetch {remote}"))?;
        }
        Ok(default.name)
    }

    /// The caller holds the repository's Git gate.
    pub async fn fast_forward_default(
        &self,
        repo: &Repository,
        default: &BranchName,
    ) -> Result<SyncedRepository> {
        let remote = upstream(repo, &git::local_ref(default))
            .await?
            .map(|(remote, _)| remote);
        let default_branch = self
            .follow_fetched_upstream(repo, default)
            .await
            .with_context(|| format!("could not refresh {default}"))?;
        Ok(SyncedRepository {
            repository_id: repo.id.clone(),
            remote,
            default_branch,
        })
    }

    pub async fn remote_heads(
        &self,
        daemon: &dyn Daemon,
        repository_id: &RepositoryId,
    ) -> Result<Vec<git::RemoteHead>> {
        let repo = daemon
            .repository(crate::repository::Selector::from(repository_id))
            .await?;
        let mut heads = Vec::new();
        for remote in &git::remote_names(&repo.path).await? {
            heads.extend(
                git::remote_heads(&repo.path, remote)
                    .await
                    .with_context(|| format!("could not list the branches of {remote}"))?,
            );
        }
        Ok(heads)
    }

    /// Whatever commits carry the changes.
    pub async fn default_branch_has_head(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<bool> {
        daemon.fetch_default_branch(workspace.repository()).await?;
        let repo = daemon
            .repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        let default = git::default_branch::locate(&repo.path, DefaultBranchLookup::Cached).await?;
        git::status::default_branch_retains_head(&workspace.path, &default).await
    }

    pub(super) async fn managed_workspace_at(&self, path: &Path) -> Result<Option<String>> {
        let workspaces = self.list_workspaces().await?;
        let path = path.to_owned();
        off_runtime::run(move || {
            Ok(workspaces
                .into_iter()
                .find(|w| std::fs::canonicalize(&w.path).is_ok_and(|managed| managed == path))
                .map(|w| w.name.into()))
        })
        .await
    }

    /// The caller holds the repository's Git gate through any creation that
    /// follows.
    pub(super) async fn refresh_branch(
        &self,
        repo: &Repository,
        branch: &BranchName,
        policy: UpstreamPolicy,
    ) -> Result<PulledBranch> {
        let following = match self.prepare_refresh(repo, branch, policy).await? {
            Refresh::Unchanged(pulled) => return Ok(pulled),
            Refresh::Follow(following) => following,
        };
        let (remote, reference) = (following.remote.as_str(), following.reference.as_str());
        let commit = fetch::with_temporary_ref(&repo.path, "pull", git_run, async |fetched| {
            let commit =
                fetch::fetch_commit(&repo.path, remote, reference, fetched, git_run).await?;
            self.advance(repo, &following, &commit).await
        })
        .await?;
        Ok(following.pulled(repo, commit))
    }

    /// Uses the last fetch without asking the remote, so the caller holds
    /// the Git gate only for local work.
    async fn follow_fetched_upstream(
        &self,
        repo: &Repository,
        branch: &BranchName,
    ) -> Result<PulledBranch> {
        let following = match self
            .prepare_refresh(repo, branch, UpstreamPolicy::AllowLocalOnly)
            .await?
        {
            Refresh::Unchanged(pulled) => return Ok(pulled),
            Refresh::Follow(following) => following,
        };
        let commit = fetched_upstream_commit(repo, &following).await?;
        let commit = self.advance(repo, &following, &commit).await?;
        Ok(following.pulled(repo, commit))
    }

    /// Refuses a checkout a refresh cannot advance. Returns the branch
    /// unchanged when there is nothing to follow.
    async fn prepare_refresh(
        &self,
        repo: &Repository,
        branch: &BranchName,
        policy: UpstreamPolicy,
    ) -> Result<Refresh> {
        let name = branch;
        let local_ref = git::local_ref(name);
        let previous_commit = git::verify_revision(&repo.path, &local_ref, git_run)
            .await
            .with_context(|| format!("repository has no local {branch} branch"))?
            .trim()
            .to_owned();
        let upstream = upstream(repo, &local_ref).await?;
        if matches!(policy, UpstreamPolicy::AllowLocalOnly)
            && upstream.is_none()
            && git::remote_names(&repo.path).await?.is_empty()
        {
            return Ok(Refresh::Unchanged(PulledBranch::unchanged(
                repo,
                name,
                previous_commit.into(),
                format!(
                    "{branch} has no upstream and the repository has no remotes; nothing to refresh"
                ),
            )));
        }
        let (remote, reference) = upstream.with_context(|| {
            format!(
                "{branch} has no branch upstream; configure it with git branch --set-upstream-to=<remote>/{branch} {branch}"
            )
        })?;
        let checkout = git::checkout_of(&repo.path, name, git::run)
            .await?
            .map(|tree| tree.path);
        if let Some(checkout) = &checkout {
            let path = off_runtime::canonicalize(checkout).await?;
            ensure!(
                self.managed_workspace_at(&path).await?.is_none(),
                "{branch} is checked out in a managed workspace; switch that workspace back to its own branch first"
            );
            clean_branch(checkout, name).await?;
        }
        Ok(Refresh::Follow(Following {
            branch: branch.to_owned(),
            local_ref,
            previous_commit: previous_commit.into(),
            remote,
            reference,
            checkout,
        }))
    }

    /// Like `pull --ff-only`. Returns where the branch ends up.
    async fn advance(
        &self,
        repo: &Repository,
        following: &Following,
        commit: &CommitId,
    ) -> Result<CommitId> {
        let Following {
            branch,
            local_ref,
            previous_commit,
            checkout,
            ..
        } = following;
        ensure!(
            git::revision(&repo.path, local_ref, git_run).await?.trim() == previous_commit,
            "{branch} changed during fetch; retry"
        );
        // A branch that is already ahead stays as it is.
        if git::is_ancestor(
            &repo.path,
            commit.as_str(),
            previous_commit.as_str(),
            git::isolated_command,
        )
        .await?
        {
            return Ok(previous_commit.clone());
        }
        ensure!(
            git::is_ancestor(
                &repo.path,
                previous_commit.as_str(),
                commit.as_str(),
                git::isolated_command
            )
            .await?,
            "{branch} and its upstream have diverged; resolve this manually before refreshing"
        );
        if let Some(checkout) = checkout {
            clean_branch(checkout, branch).await?;
            git::branch::fast_forward(checkout, commit.as_str()).await?;
        } else {
            git::fast_forward_local(&repo.path, commit.as_str(), local_ref).await?;
        }
        Ok(commit.clone())
    }

    /// Includes branches never fetched locally.
    pub async fn branches(
        &self,
        daemon: &dyn Daemon,
        repository: impl Into<crate::repository::Selector>,
    ) -> Result<Vec<Branch>> {
        let repo = daemon.repository(repository.into()).await?;
        daemon.ensure_available(&repo.id).await?;
        git::existing_branch::list(&repo.path).await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn open_branch(
        &self,
        daemon: &dyn Daemon,
        repository: impl Into<crate::repository::Selector>,
        selector: &str,
        git_profile: Option<&str>,
        path: Option<std::path::PathBuf>,
        base: Option<String>,
    ) -> Result<OpenedWorkspace> {
        let new = super::NewWorkspace {
            git_profile,
            path,
            ..Default::default()
        };
        self.open_branch_with(daemon, repository, selector, base, new)
            .await
    }

    /// Reopens the workspace that owns the branch, or creates one as `new`
    /// says.
    pub async fn open_branch_with(
        &self,
        daemon: &dyn Daemon,
        repository: impl Into<crate::repository::Selector>,
        selector: &str,
        base: Option<String>,
        new: super::NewWorkspace<'_>,
    ) -> Result<OpenedWorkspace> {
        self.open_branch_restoring(daemon, repository, selector, base, new, None)
            .await
    }

    /// Opens a new worktree at the removal checkpoint a restore `restores`,
    /// with the unsaved files it keeps, before it can be used. A reopened
    /// workspace gets none.
    pub async fn open_branch_restoring(
        &self,
        daemon: &dyn Daemon,
        repository: impl Into<crate::repository::Selector>,
        selector: &str,
        base: Option<String>,
        new: super::NewWorkspace<'_>,
        restores: Option<shoal_ids::checkpoint_numbers::CheckpointNumber>,
    ) -> Result<OpenedWorkspace> {
        let (repo, git) = daemon.lock_repository(repository.into()).await?;
        let options = NewWorktreeOptions {
            git_profile: new.git_profile,
            path: new.path.as_deref(),
            base: base.as_deref(),
            disposable: new.options.disposable,
        };
        let branch = match self.open_target(daemon, &repo, selector, options).await? {
            OpenTarget::Reopen(workspace) => {
                self.reopen_owned_workspace(
                    daemon,
                    &workspace,
                    workspace.branch().map(BranchName::as_str),
                )
                .await?;
                return Ok(OpenedWorkspace {
                    workspace: *workspace,
                    reused: true,
                });
            }
            OpenTarget::Create(branch) => branch,
        };
        let workspace = self
            .create_branch_workspace(
                daemon,
                &repo,
                branch.name.to_string(),
                branch.name.clone(),
                super::creation::WorkspaceSource::Existing {
                    branch,
                    base,
                    restores,
                },
                new,
                git,
            )
            .await?;
        Ok(OpenedWorkspace {
            workspace,
            reused: false,
        })
    }

    /// Reopening allows no new-worktree options.
    pub(super) async fn open_target(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        selector: &str,
        options: NewWorktreeOptions<'_>,
    ) -> Result<OpenTarget> {
        let workspaces = self.list_workspaces().await?;
        // A new branch exists only once its pending worktree is created.
        let local = git::strip_local(selector).unwrap_or(selector);
        let pending = (workspaces.iter()).find(|w| {
            w.checkout.is_pending()
                && w.repository() == &repo.id
                && w.branch().is_some_and(|branch| branch.as_str() == local)
        });
        if let Some(workspace) = pending {
            check_reopen_options(workspace, options).await?;
            return Ok(OpenTarget::Reopen(Box::new(workspace.clone())));
        }
        let branch = self.select_existing_branch(daemon, repo, selector).await?;
        let Some(workspace) = workspaces
            .into_iter()
            .find(|w| w.repository() == &repo.id && w.branch() == Some(&branch.name))
        else {
            ensure_not_checked_out(&repo.path, &branch.name).await?;
            return Ok(OpenTarget::Create(branch));
        };
        check_reopen_options(&workspace, options).await?;
        Ok(OpenTarget::Reopen(Box::new(workspace)))
    }

    pub async fn opening_owner(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        selector: &str,
    ) -> Result<Option<Workspace>> {
        let options = NewWorktreeOptions::default();
        Ok(
            match self.open_target(daemon, repo, selector, options).await? {
                OpenTarget::Reopen(workspace) => Some(*workspace),
                OpenTarget::Create(_) => None,
            },
        )
    }

    async fn select_existing_branch(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        selector: &str,
    ) -> Result<Branch> {
        let local = git::strip_local(selector).unwrap_or(selector);
        let local_ref = git::local_ref(&local.into());
        let branch = if git::strip_remote(selector).is_none()
            && git::ref_exists(&repo.path, &local_ref).await?
        {
            Branch {
                name: local.into(),
                remote: None,
            }
        } else {
            let matches: Vec<_> = self
                .branches(daemon, &repo.id)
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
        self.validate_existing_branch(repo, &branch).await?;
        Ok(branch)
    }

    /// Checks that the branch tracks its requested remote.
    async fn validate_existing_branch(&self, repo: &Repository, branch: &Branch) -> Result<()> {
        let name = &branch.name;
        ensure!(
            !git::branch::is_reserved_name(name.as_str()),
            "branch name is ambiguous as a Git revision: {name}"
        );
        git::check_branch_name(name.as_str())?;
        if let Some(remote) = &branch.remote {
            let local = name.clone();
            let local_exists = git::ref_exists(&repo.path, &git::local_ref(&local)).await?;
            if local_exists {
                let upstream = git::existing_branch::upstream_ref(&repo.path, &local).await?;
                ensure!(
                    upstream.trim() == git::remote_ref(remote, name.as_str()),
                    "local branch {name} already exists and does not track {remote}/{name}; select the local branch explicitly"
                );
            }
        }
        Ok(())
    }

    pub(super) async fn materialize_branch(
        &self,
        repo: &Repository,
        branch: &Branch,
    ) -> Result<()> {
        if let Some(remote) = &branch.remote {
            let name = branch.name.clone();
            let tracking = fetch::materialize_branch(&repo.path, remote, &name).await?;
            let local = git::local_ref(&name);
            if git::ref_exists(&repo.path, &local).await? {
                self.refresh_branch(repo, &branch.name, UpstreamPolicy::Required)
                    .await?;
            } else {
                git::existing_branch::track(&repo.path, &name, &tracking).await?;
            }
        }
        Ok(())
    }
}

/// As the last fetch left it.
async fn fetched_upstream_commit(repo: &Repository, following: &Following) -> Result<CommitId> {
    let branch = &following.branch;
    let tracking = git::existing_branch::upstream_ref(&repo.path, branch).await?;
    let tracking = tracking.trim();
    ensure!(
        !tracking.is_empty(),
        "{branch} has no remote-tracking branch; check the fetch refspec of {}",
        following.remote
    );
    git::resolve_commit(&repo.path, tracking, git_run)
        .await
        .with_context(|| format!("{branch}'s remote-tracking branch {tracking} was never fetched"))
}

#[cfg(test)]
mod tests;
