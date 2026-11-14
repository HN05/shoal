//! Refresh: bring a workspace up to date with what it was made from while it
//! holds nothing of its own. A branch moves only by fast-forward, and work
//! of its own is refused, never carried over.
use anyhow::{Context, Result, bail, ensure};
use shoal_refs::CommitId;

use super::Manager;
use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::scope::Caller,
    git::{self, default_branch::DefaultBranchLookup},
    model::{PresentSpace, Refreshed, Repository, Workspace},
    state::WorkspaceState,
};

/// What a workspace was made from, which a refresh brings it up to date
/// with.
enum Target {
    /// Its branch's upstream, the branch `reference` on `remote`.
    Upstream { remote: String, reference: String },
    /// The revision its branch builds on, resolved as `--base` resolves it:
    /// its base workspace's branch, or the branch its recorded base names.
    Base(String),
    /// The local default branch, synced first.
    DefaultBranch,
}

/// The commit a refresh moves a workspace to, and how Git or Shoal names
/// what holds it.
struct Found {
    commit: CommitId,
    label: String,
}

/// The most commit subjects and changed paths a refusal lists.
const LISTED: usize = 5;

impl Manager {
    /// `shoal refresh`: bring the workspace `selector` names up to date with
    /// what it was made from, refusing one that holds work of its own, or in
    /// which an execution other than `caller`'s own runs.
    pub async fn refresh_workspace(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<Refreshed> {
        let workspace = match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => workspace,
            PresentSpace::Chat(_) => bail!("a chat has no worktree to refresh"),
        };
        let target = self.refresh_target(&workspace).await?;
        if matches!(target, Target::DefaultBranch) {
            // The sync takes the repository's Git gate itself.
            self.sync_repository(&(&workspace.repository_id).into())
                .await
                .context("could not sync the default branch to refresh to")?;
        }
        let (repo, _git) = self.lock_repository(&workspace.repository_id).await?;
        let reserved = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Updating)
            .await?;
        // A rename may have finished while this request waited.
        let workspace = self.workspace_by_id(&workspace.id).await?;
        let own = caller.and_then(Caller::execution_id);
        let result = self
            .refresh_reserved(&repo, &workspace, reserved, target, own)
            .await;
        self.set_state(&workspace.id, reserved, workspace.error.clone())
            .await?;
        let refreshed = result?;
        self.touch(&workspace.id).await;
        Ok(Refreshed {
            workspace: self.workspace_by_id(&workspace.id).await?,
            ..refreshed
        })
    }

    /// What the `workspace` record says it was made from.
    async fn refresh_target(&self, workspace: &Workspace) -> Result<Target> {
        let repo = self.repository(&workspace.repository_id).await?;
        let Some(branch) = &workspace.branch else {
            bail!(
                "workspace {} is at a commit, which refresh does not move yet",
                workspace.name
            );
        };
        if let Some((remote, reference)) =
            git::repo::upstream(&repo, &git::local_ref(branch)).await?
        {
            return Ok(Target::Upstream { remote, reference });
        }
        if let Some(base) = (workspace.base_workspace.as_ref()).and_then(|base| base.branch.clone())
        {
            return Ok(Target::Base(base.into()));
        }
        let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await;
        Ok(match (&workspace.base_ref, default) {
            (Some(base), Ok(default)) if *base != git::local_ref(&default) => {
                Target::Base(base.clone())
            }
            _ => Target::DefaultBranch,
        })
    }

    /// Refresh the reserved `workspace`, which was in `state`, to `target`,
    /// once nothing but the execution `own` runs in it.
    async fn refresh_reserved(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        state: WorkspaceState,
        target: Target,
        own: Option<&ExecutionId>,
    ) -> Result<Refreshed> {
        self.ensure_refreshable(workspace, state, own).await?;
        let path = &workspace.path;
        let from = git::resolve_commit(path, "HEAD", git::run_isolated).await?;
        let found = self.find_target(repo, target).await?;
        let up_to_date =
            git::is_ancestor(path, found.commit.as_str(), "HEAD", git::isolated_command).await?;
        if !up_to_date {
            refuse_own_work(workspace, found.commit.as_str(), &found.label).await?;
            self.fast_forward_to(workspace, found.commit.as_str())
                .await?;
        }
        Ok(Refreshed {
            workspace: workspace.clone(),
            target: found.label,
            to: git::resolve_commit(path, "HEAD", git::run_isolated).await?,
            from,
            up_to_date,
            setup: false,
        })
    }

    /// Refuse to refresh `workspace`, which was in `state`, unless it is
    /// ready, its worktree verifies with no Git operation under way, and no
    /// execution but `own` runs in it.
    async fn ensure_refreshable(
        &self,
        workspace: &Workspace,
        state: WorkspaceState,
        own: Option<&ExecutionId>,
    ) -> Result<()> {
        ensure!(
            state == WorkspaceState::Ready,
            "workspace {} is {state}; refresh it once it is ready",
            workspace.name
        );
        self.ensure_no_other_execution(workspace, own).await?;
        self.verify_worktree(workspace).await?;
        ensure!(
            !git::status::operation_in_progress(&workspace.path).await?,
            "a Git operation is under way in workspace {}; finish or abort it first",
            workspace.name
        );
        Ok(())
    }

    /// The commit `target` names now, fetching what it names on a remote.
    async fn find_target(&self, repo: &Repository, target: Target) -> Result<Found> {
        let (revision, label) = match target {
            Target::Upstream { remote, reference } => {
                let commit = git::fetch::reference(&repo.path, &remote, &reference).await?;
                let branch = git::strip_local(&reference).unwrap_or(&reference);
                let label = format!("{remote}/{branch}");
                return Ok(Found { commit, label });
            }
            Target::Base(base) => {
                let revision = self.new_branch_base(repo, Some(&base)).await?;
                (revision, short_ref(&base).to_owned())
            }
            Target::DefaultBranch => {
                let default =
                    git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
                (git::local_ref(&default), default.to_string())
            }
        };
        let commit = git::resolve_commit(&repo.path, &revision, git::run_isolated)
            .await
            .with_context(|| format!("{label} is not a commit here"))?;
        Ok(Found { commit, label })
    }
}

/// Refuse `workspace` when it holds commits that `target`, which `label`
/// names, lacks, or uncommitted changes, saying what they are and what deals
/// with them.
async fn refuse_own_work(workspace: &Workspace, target: &str, label: &str) -> Result<()> {
    let path = &workspace.path;
    let mut held = Vec::new();
    let commits = git::status::commits_since(path, target).await?;
    if commits > 0 {
        let subjects = git::status::subjects_since(path, target, LISTED).await?;
        held.push(format!(
            "{commits} commit(s) {label} lacks ({}); move them onto it with git rebase {label}",
            listed(&subjects, commits as usize)
        ));
    }
    let status = git::status::porcelain(path).await?;
    let changed: Vec<&str> = status
        .lines()
        .map(|line| line.get(3..).unwrap_or(line))
        .collect();
    if !changed.is_empty() {
        held.push(format!(
            "uncommitted changes to {}; commit, stash or discard them",
            listed(&changed, changed.len())
        ));
    }
    ensure!(
        held.is_empty(),
        "workspace {} holds work of its own, which refresh never carries over: {}",
        workspace.name,
        held.join("; and ")
    );
    Ok(())
}

/// The first of `items`, of `total` in all, joined, with how many more
/// there are.
fn listed(items: &[impl AsRef<str>], total: usize) -> String {
    let shown: Vec<&str> = items.iter().take(LISTED).map(AsRef::as_ref).collect();
    let more = total.saturating_sub(shown.len());
    match more {
        0 => shown.join(", "),
        more => format!("{}, and {more} more", shown.join(", ")),
    }
}

/// A branch's name as Git shortens its full ref.
fn short_ref(reference: &str) -> &str {
    git::strip_local(reference)
        .or_else(|| git::strip_remote(reference))
        .unwrap_or(reference)
}

#[cfg(test)]
mod tests;
