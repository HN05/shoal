//! Branch allocation, existing-branch workspaces and default-branch refreshes
//! against workspace records. Adoption is explicit.
use std::path::Path;

use anyhow::{Context, Result, ensure};

use super::Manager;
use crate::{
    fsutil::off_runtime,
    git::{
        self,
        default_branch::DefaultBranchLookup,
        existing_branch::{Branch, ensure_not_checked_out},
        fetch,
        repo::{allocate_branch, clean_branch, upstream},
        run_isolated as git_run, worktrunk,
    },
    model::{OpenedWorkspace, PulledBranch, Repository, SyncedRepository, Workspace},
};

/// Options that apply only when opening creates a new worktree.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NewWorktreeOptions<'a> {
    pub git_profile: Option<&'a str>,
    pub path: Option<&'a Path>,
    pub base: Option<&'a str>,
    pub disposable: bool,
}

/// What opening an existing branch does.
pub(super) enum OpenTarget {
    /// Reopen the workspace that owns the branch.
    Reopen(Box<Workspace>),
    /// Create a worktree for the branch.
    Create(Branch),
}

/// Refuse new-worktree options for a workspace that opening reuses, except a
/// path naming where it already is.
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

/// What refreshing a branch does.
enum Refresh {
    /// Nothing: the branch has no upstream to follow.
    Unchanged(PulledBranch),
    Follow(Following),
}

/// A local branch about to follow its upstream.
struct Following {
    branch: String,
    local_ref: String,
    previous_commit: String,
    remote: String,
    /// The upstream branch's ref on `remote`.
    reference: String,
    /// The worktree that has the branch checked out, if any.
    checkout: Option<std::path::PathBuf>,
}

impl Following {
    fn pulled(self, repo: &Repository, commit: String) -> PulledBranch {
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
                .filter_map(|workspace| workspace.branch),
        );
        Ok(allocate_branch(name, &taken))
    }

    /// Fetch every remote that holds a default-branch ref merge checks read:
    /// the local default branch's upstream and the default remote, so they
    /// see what the forge merged; local branches stay unchanged. Returns the
    /// default branch. The caller holds the repository's fetch gate.
    pub(crate) async fn fetch_default_remotes(&self, repo: &Repository) -> Result<String> {
        let default =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Discover).await?;
        let mut remotes = Vec::new();
        let local = git::local_ref(&default);
        if git::ref_exists(&repo.path, &local, git::isolated_command).await?
            && let Some((remote, _)) = upstream(repo, &local).await?
        {
            remotes.push(remote);
        }
        if let Some(remote) = crate::git::default_branch::remote(&repo.path).await?
            && !remotes.contains(&remote)
        {
            remotes.push(remote);
        }
        for remote in remotes {
            fetch::remote(&repo.path, &remote)
                .await
                .with_context(|| format!("could not fetch {remote}"))?;
        }
        Ok(default)
    }

    /// Fast-forward the local default branch to its upstream as
    /// [`Self::fetch_default_remotes`] fetched it. The caller holds the
    /// repository's Git gate.
    pub(crate) async fn fast_forward_default(
        &self,
        repo: &Repository,
        default: &str,
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

    /// The branches every remote of the repository advertises now.
    pub async fn remote_heads(&self, repository_id: &str) -> Result<Vec<git::RemoteHead>> {
        let repo = self.repository(repository_id).await?;
        let mut heads = Vec::new();
        for remote in git::remotes(&repo.path, git::run_isolated).await?.lines() {
            heads.extend(
                git::remote_heads(&repo.path, remote)
                    .await
                    .with_context(|| format!("could not list the branches of {remote}"))?,
            );
        }
        Ok(heads)
    }

    /// Whether the freshly fetched default branch already has the
    /// workspace's HEAD changes, whatever commits carry them.
    pub(crate) async fn default_branch_has_head(&self, workspace: &Workspace) -> Result<bool> {
        self.fetch_default_branch(&workspace.repository_id).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        git::status::default_branch_retains_head(&workspace.path, &default).await
    }

    /// Match a canonical checkout path against recorded workspace locations.
    pub(super) async fn managed_workspace_at(&self, path: &Path) -> Result<Option<String>> {
        let workspaces = self.list_workspaces().await?;
        let path = path.to_owned();
        off_runtime::run(move || {
            Ok(workspaces
                .into_iter()
                .find(|w| std::fs::canonicalize(&w.path).is_ok_and(|managed| managed == path))
                .map(|w| w.name))
        })
        .await
    }

    /// Fetch a local branch's upstream and fast-forward the branch to it. The
    /// caller holds the repository's Git gate through any subsequent creation.
    pub(super) async fn refresh_branch(
        &self,
        repo: &Repository,
        branch: &str,
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

    /// Fast-forward a local branch to its remote-tracking branch as the last
    /// fetch left it, without asking the remote, so the caller holds the
    /// repository's Git gate only for local work.
    async fn follow_fetched_upstream(
        &self,
        repo: &Repository,
        branch: &str,
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

    /// The branch's upstream and checkout, refusing a checkout a refresh
    /// cannot advance, or the unchanged branch when there is nothing to follow.
    async fn prepare_refresh(
        &self,
        repo: &Repository,
        branch: &str,
        policy: UpstreamPolicy,
    ) -> Result<Refresh> {
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
            return Ok(Refresh::Unchanged(PulledBranch::unchanged(
                repo,
                branch,
                previous_commit,
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
        let checkout = git::checkout_of(&repo.path, branch, git::run)
            .await?
            .map(|tree| tree.path);
        if let Some(checkout) = &checkout {
            let path = off_runtime::canonicalize(checkout).await?;
            ensure!(
                self.managed_workspace_at(&path).await?.is_none(),
                "{branch} is checked out in a managed workspace; switch that workspace back to its own branch first"
            );
            clean_branch(checkout, branch).await?;
        }
        Ok(Refresh::Follow(Following {
            branch: branch.to_owned(),
            local_ref,
            previous_commit,
            remote,
            reference,
            checkout,
        }))
    }

    /// Fast-forward the branch to `commit`, which its upstream has, like
    /// pull --ff-only; returns where the branch ends up.
    async fn advance(
        &self,
        repo: &Repository,
        following: &Following,
        commit: &str,
    ) -> Result<String> {
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
        // An already-ahead branch stays untouched.
        if git::is_ancestor(&repo.path, commit, previous_commit, git::isolated_command).await? {
            return Ok(previous_commit.clone());
        }
        ensure!(
            git::is_ancestor(&repo.path, previous_commit, commit, git::isolated_command).await?,
            "{branch} and its upstream have diverged; resolve this manually before refreshing"
        );
        if let Some(checkout) = checkout {
            clean_branch(checkout, branch).await?;
            git::branch::fast_forward(checkout, commit).await?;
        } else {
            git::fast_forward_local(&repo.path, commit, local_ref).await?;
        }
        Ok(commit.to_owned())
    }

    /// Query advertised remote heads, including branches never fetched locally.
    pub async fn branches(
        &self,
        repository: impl Into<crate::repository::Selector>,
    ) -> Result<Vec<Branch>> {
        let repo = self.repository(repository).await?;
        self.ensure_repository_available(&repo.id).await?;
        git::existing_branch::list(&repo.path).await
    }

    #[cfg(test)]
    pub async fn open_branch(
        &self,
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
        self.open_branch_with(repository, selector, base, new).await
    }

    /// Open the branch `selector` names in `repository`, reopening the
    /// workspace that owns it or creating one where and as `new` says.
    pub(crate) async fn open_branch_with(
        &self,
        repository: impl Into<crate::repository::Selector>,
        selector: &str,
        base: Option<String>,
        new: super::NewWorkspace<'_>,
    ) -> Result<OpenedWorkspace> {
        self.open_branch_restoring(repository, selector, base, new, None)
            .await
    }

    /// [`Self::open_branch_with`], writing a restore's files into a new
    /// worktree before it can be used; a reopened workspace gets none.
    pub(super) async fn open_branch_restoring(
        &self,
        repository: impl Into<crate::repository::Selector>,
        selector: &str,
        base: Option<String>,
        new: super::NewWorkspace<'_>,
        files: Option<super::restore::RestoredFiles>,
    ) -> Result<OpenedWorkspace> {
        let (repo, _guard) = self.lock_repository(repository).await?;
        let options = NewWorktreeOptions {
            git_profile: new.git_profile,
            path: new.path.as_deref(),
            base: base.as_deref(),
            disposable: new.options.disposable,
        };
        let branch = match self.open_target(&repo, selector, options).await? {
            OpenTarget::Reopen(workspace) => {
                self.reopen_owned_workspace(&workspace, workspace.branch.as_deref())
                    .await?;
                return Ok(OpenedWorkspace {
                    workspace: *workspace,
                    reused: true,
                });
            }
            OpenTarget::Create(branch) => branch,
        };
        let name = branch.name.clone();
        let workspace = self
            .create_branch_workspace(
                &repo,
                name.clone(),
                name,
                super::WorkspaceSource::Existing {
                    branch,
                    base,
                    files,
                },
                new,
            )
            .await?;
        Ok(OpenedWorkspace {
            workspace,
            reused: false,
        })
    }

    /// What opening `selector` does: reopen the workspace that owns the branch,
    /// which new-worktree options cannot change, or create one from it.
    pub(super) async fn open_target(
        &self,
        repo: &Repository,
        selector: &str,
        options: NewWorktreeOptions<'_>,
    ) -> Result<OpenTarget> {
        let branch = self.select_existing_branch(repo, selector).await?;
        let Some(workspace) = self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|w| w.repository_id == repo.id && w.branch.as_ref() == Some(&branch.name))
        else {
            ensure_not_checked_out(&repo.path, &branch.name).await?;
            return Ok(OpenTarget::Create(branch));
        };
        check_reopen_options(&workspace, options).await?;
        Ok(OpenTarget::Reopen(Box::new(workspace)))
    }

    /// Resolve and validate the local or remote branch selected by the caller.
    async fn select_existing_branch(&self, repo: &Repository, selector: &str) -> Result<Branch> {
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
        self.validate_existing_branch(repo, &branch).await?;
        Ok(branch)
    }

    /// Check that a selected branch is usable and tracks its requested remote.
    async fn validate_existing_branch(&self, repo: &Repository, branch: &Branch) -> Result<()> {
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
        Ok(())
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

/// The commit the branch's remote-tracking branch holds, as the last fetch
/// left it.
async fn fetched_upstream_commit(repo: &Repository, following: &Following) -> Result<String> {
    let branch = following.branch.as_str();
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
