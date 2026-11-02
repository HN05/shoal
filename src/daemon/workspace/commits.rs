//! Workspaces at a commit: worktrees at a detached HEAD with no branch of
//! their own, created from a revision or a PR's head. Worktrunk only creates
//! worktrees on a branch, so plain Git creates theirs; removal goes through
//! Worktrunk like any workspace's.
use anyhow::{Context, Result};

use super::{Manager, derive_workspace_name};
use crate::{
    git,
    model::{MadeFor, Repository, StartingCommit, Workspace},
    protocol::CommitStart,
    state::WorkspaceState,
};

/// The commit a workspace at a commit starts at, what it was made for, and
/// the name it derives from them.
struct Start {
    at: StartingCommit,
    name: String,
}

impl Manager {
    /// Create a workspace at the commit `start` names in `repository`: a
    /// child of `parent`, or without one a top-level workspace given to
    /// `chat`, or else to the user. It runs setup like any workspace unless
    /// the caller skips it.
    pub async fn create_commit_workspace(
        &self,
        repository: crate::repository::Selector,
        parent: Option<String>,
        start: CommitStart,
        chat: Option<String>,
    ) -> Result<Workspace> {
        let parent = match parent {
            Some(parent) => Some(self.workspace(&parent).await?.id),
            None => None,
        };
        let (repo, _guard) = self.lock_repository(repository).await?;
        let start = self.resolve_start(&repo, start).await?;
        let name = self.free_workspace_name(&start.name).await?;
        let path = self.new_workspace_path(&repo, &name, None).await?;
        let workspace = Workspace {
            base_commit: Some(start.at.commit.clone()),
            at: Some(start.at),
            ..Workspace::new_record(repo.id.clone(), name, path, None, WorkspaceState::Preparing)
        };
        self.insert_workspace(workspace.clone(), None, parent)
            .await?;
        let result = self.materialize_commit_worktree(&repo, &workspace).await;
        let created = self.settle_creation(&workspace, result).await?;
        self.give_to_chat(&created.id, chat).await
    }

    /// Check out the recorded starting commit and configure the worktree,
    /// returning whether setup is still needed.
    async fn materialize_commit_worktree(
        &self,
        repo: &Repository,
        workspace: &Workspace,
    ) -> Result<bool> {
        let at = workspace
            .at
            .as_ref()
            .context("workspace records no starting commit")?;
        git::detached::add_workspace(&repo.path, &workspace.path, &at.commit).await?;
        self.configure_worktree(repo, workspace, None, None).await
    }

    /// Resolve `start` in `repo`: the commit, what it was made for and the
    /// name it gives the workspace.
    async fn resolve_start(&self, repo: &Repository, start: CommitStart) -> Result<Start> {
        match start {
            CommitStart::Revision(base) => self.resolve_revision(repo, base.as_deref()).await,
            CommitStart::PullHead {
                number,
                reference,
                commit,
            } => pull_head(repo, number, &reference, commit).await,
        }
    }

    /// Resolve a revision as a branch workspace's `--base` resolves, fetching
    /// a remote branch it names; a tag or commit that does not resolve makes
    /// `origin` fetch with its tags once before trying again.
    async fn resolve_revision(&self, repo: &Repository, base: Option<&str>) -> Result<Start> {
        let revision = self.new_branch_base(repo, base).await?;
        let commit = match git::resolve_commit(&repo.path, &revision, git::run_isolated).await {
            Ok(commit) => commit,
            Err(error) => {
                if git::fetch::with_tags(&repo.path, "origin").await.is_err() {
                    return Err(error.context(format!("{revision} is not a commit here")));
                }
                git::resolve_commit(&repo.path, &revision, git::run_isolated)
                    .await
                    .with_context(|| format!("{revision} is not a commit here or on origin"))?
            }
        };
        let reference = git::symbolic_revision(&repo.path, &revision, git::run_isolated).await?;
        Ok(named_start(commit, base, reference.trim_end_matches('\n')))
    }

    /// `name`, or the first of `name-2`, `name-3`, ... that nothing holds.
    async fn free_workspace_name(&self, name: &str) -> Result<String> {
        let name = derive_workspace_name(name);
        self.store
            .run(move |db| crate::daemon::store::free_name(db, &name))
            .await
    }
}

/// PR `number`'s head, which `origin` serves under `reference`, fetched and
/// checked to be the `commit` the forge reported.
async fn pull_head(
    repo: &Repository,
    number: u64,
    reference: &str,
    commit: String,
) -> Result<Start> {
    let fetched = git::fetch::reference(&repo.path, "origin", reference).await?;
    anyhow::ensure!(
        fetched == commit,
        "PR #{number} moved while its head was fetched; run the command again"
    );
    Ok(Start {
        at: StartingCommit {
            commit,
            made_for: Some(MadeFor::Pr(number)),
        },
        name: format!("pr-{number}"),
    })
}

/// What a revision that resolved to `commit` names: a tag by its name, a
/// branch as `base` gave it, or the default branch when `base` named none,
/// with the workspace named after the tag, or the branch and the short
/// commit; a bare commit is made for nothing and named by its short commit.
fn named_start(commit: String, base: Option<&str>, reference: &str) -> Start {
    let short = commit[..commit.len().min(7)].to_owned();
    let branch = reference.strip_prefix(git::LOCAL_REFS).or_else(|| {
        reference
            .strip_prefix("refs/remotes/")
            .and_then(|remote| remote.split_once('/'))
            .map(|(_, branch)| branch)
    });
    let (name, made_for) = match (reference.strip_prefix("refs/tags/"), branch) {
        (Some(tag), _) => (tag.to_owned(), Some(tag.to_owned())),
        (None, Some(branch)) => (
            format!("{branch}-{short}"),
            Some(base.unwrap_or(branch).to_owned()),
        ),
        (None, None) => (short, None),
    };
    Start {
        at: StartingCommit {
            commit,
            made_for: made_for.map(MadeFor::Ref),
        },
        name,
    }
}

#[cfg(test)]
mod tests;
