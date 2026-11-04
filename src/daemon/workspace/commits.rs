//! Workspaces at a commit: worktrees at a detached HEAD with no branch of
//! their own, created from a revision or a PR's head. Worktrunk only creates
//! worktrees on a branch, so plain Git creates theirs; removal goes through
//! Worktrunk like any workspace's.
use anyhow::{Context, Result};

use super::{Manager, derive_workspace_name, restore::RestoredFiles};
use crate::{
    git,
    model::{
        MadeFor, OpenedWorkspace, RemovedWorkspace, Repository, SpaceKind, StartingCommit,
        Workspace,
    },
    protocol::{CommitStart, CreationOptions, PullHead},
    state::WorkspaceState,
};

/// The commit a workspace at a commit starts at, what it was made for, and
/// the name it derives from them.
struct Start {
    at: StartingCommit,
    name: String,
    /// The ref its diff compares with; none compares with `at`.
    diff_ref: Option<String>,
}

impl Manager {
    /// Create a workspace at the commit `start` names in `repository`: a
    /// child of the space `parent`, a chat's ID or a workspace, or without
    /// one a top-level workspace of the user's. It runs setup like any
    /// workspace unless the caller skips it.
    pub async fn create_commit_workspace(
        &self,
        repository: crate::repository::Selector,
        parent: Option<String>,
        start: CommitStart,
        options: CreationOptions,
    ) -> Result<Workspace> {
        let parent = match parent {
            Some(parent) => Some(self.parent_space(&parent).await?),
            None => None,
        };
        // Checked before resolving the start, which may fetch: a scoped
        // process reaches only its own repository.
        self.ensure_parent_repository(&repository, parent.as_deref())
            .await?;
        let (repo, _guard) = self.lock_repository(repository).await?;
        self.create_at_start(&repo, parent, start, options).await
    }

    /// The ID of the space `parent` names: a chat's ID, or a workspace's
    /// name or ID.
    async fn parent_space(&self, parent: &str) -> Result<String> {
        let id = parent.to_owned();
        let kind = self
            .store
            .run(move |db| crate::daemon::spaces::space_kind(db, &id))
            .await?;
        Ok(match kind {
            Some(SpaceKind::Chat) => parent.to_owned(),
            _ => self.workspace(parent).await?.id,
        })
    }

    /// Open PR `url`'s `head` in a workspace at a commit in `repository`:
    /// the one made for or linked to the PR, moved to `head` when the PR's
    /// head moved, so there is at most one per PR, or else a new one below
    /// the space `parent`, or a top-level one of the user's. A `confined`
    /// parent, whose own agent asks, takes over no workspace outside its
    /// subtree.
    pub async fn open_pull_head(
        &self,
        repository: crate::repository::Selector,
        url: String,
        head: PullHead,
        parent: Option<String>,
        options: CreationOptions,
        confined: bool,
    ) -> Result<OpenedWorkspace> {
        self.ensure_parent_repository(&repository, parent.as_deref())
            .await?;
        let (repo, _guard) = self.lock_repository(repository).await?;
        if let Some(workspace) = self.pull_workspace(&repo.id, &url, head.number).await? {
            if let Some(parent) = parent.as_deref().filter(|_| confined) {
                anyhow::ensure!(
                    self.within_subtree(parent, &workspace.id).await?,
                    "{} is outside the space opening it; ask the user to open it",
                    workspace.name
                );
            }
            self.move_to_pull_head(&repo, &workspace, &head).await?;
            if let Some(base) = head.base {
                self.record_diff_ref(&workspace.id, base).await?;
            }
            // Its last opener owns it, as a reopened branch's does; a
            // workspace's child stays its parent's.
            let workspace = match workspace.parent {
                Some(parent) if parent.kind == SpaceKind::Workspace => {
                    self.workspace(&workspace.id).await?
                }
                _ => self.give_to_parent(&workspace.id, parent).await?,
            };
            return Ok(OpenedWorkspace {
                workspace,
                reused: true,
            });
        }
        let start = CommitStart::PullHead(head);
        Ok(OpenedWorkspace {
            workspace: self.create_at_start(&repo, parent, start, options).await?,
            reused: false,
        })
    }

    /// Create a workspace at `start` in `repo`, whose Git gate the caller
    /// holds, as [`Self::create_commit_workspace`] does.
    async fn create_at_start(
        &self,
        repo: &Repository,
        parent: Option<String>,
        start: CommitStart,
        options: CreationOptions,
    ) -> Result<Workspace> {
        // Recorded below its parent, a workspace or a chat, with its
        // record, so one whose creation fails is still there.
        let start = self.resolve_start(repo, start).await?;
        let head = start.at.commit.clone();
        let workspace = self
            .record_commit_workspace(repo, &start.name, start.at, start.diff_ref, parent, options)
            .await?;
        let result = self
            .materialize_commit_worktree(repo, &workspace, &head, None)
            .await;
        self.settle_creation(&workspace, result).await
    }

    /// Create the workspace CI job `name` runs in: at `commit`, fetched
    /// from the ref `reference` `origin` serves it under, or else by
    /// itself, made for nothing, so no PR reuses it, and disposable, without
    /// setup; the seed clone still happens.
    pub(crate) async fn create_job_workspace(
        &self,
        repository_id: &str,
        name: &str,
        commit: &str,
        reference: Option<&str>,
    ) -> Result<Workspace> {
        let (repo, _guard) = self.lock_repository(repository_id).await?;
        fetch_job_commit(&repo, commit, reference).await?;
        let at = StartingCommit {
            commit: commit.to_owned(),
            made_for: None,
        };
        let options = CreationOptions {
            skip_setup: true,
            disposable: true,
        };
        let workspace = self
            .record_commit_workspace(&repo, name, at, None, None, options)
            .await?;
        let result = self
            .materialize_commit_worktree(&repo, &workspace, commit, None)
            .await;
        self.settle_creation(&workspace, result).await
    }

    /// The workspace at a commit of `repository_id` made for PR `number` or
    /// linked to it at `url`.
    async fn pull_workspace(
        &self,
        repository_id: &str,
        url: &str,
        number: u64,
    ) -> Result<Option<Workspace>> {
        let made_for = Some(MadeFor::Pr(number));
        Ok(self.list_workspaces().await?.into_iter().find(|workspace| {
            workspace.repository_id == repository_id
                && workspace.at.as_ref().is_some_and(|at| {
                    at.made_for == made_for || workspace.links.prs.iter().any(|pr| pr == url)
                })
        }))
    }

    /// Move `workspace`, whose repository's Git gate the caller holds, to
    /// the PR head `head` and record it as its starting commit, unless it is
    /// there already. The lifecycle reservation excludes removal, restores
    /// and new commands meanwhile.
    async fn move_to_pull_head(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        head: &PullHead,
    ) -> Result<()> {
        // Reopening is activity, which restarts its idle cleanup timers.
        self.reopen_owned_workspace(workspace, None).await?;
        let at = workspace
            .at
            .as_ref()
            .context("workspace records no starting commit")?;
        if at.commit == head.commit {
            return Ok(());
        }
        let reserved = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Updating)
            .await?;
        let result = self
            .replace_pull_head(repo, workspace, &at.commit, head)
            .await;
        self.set_state(&workspace.id, reserved, workspace.error.clone())
            .await?;
        result
    }

    /// Move the reserved `workspace`, which started at `start`, to the PR
    /// head `head`. One running commands, or with commits or changes of its
    /// own, is refused, as a branch workspace that diverged from its PR is.
    async fn replace_pull_head(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        start: &str,
        head: &PullHead,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.workspace_has_executions(&workspace.id).await?,
            "workspace {} runs commands; stop them before PR #{}'s new head replaces its checkout",
            workspace.name,
            head.number
        );
        let current = crate::daemon::links::pr::current_head(workspace).await?;
        anyhow::ensure!(
            current == start && git::status::porcelain(&workspace.path).await?.is_empty(),
            "workspace {} has commits or changes of its own since PR #{}'s earlier head; remove it to open the PR's new head",
            workspace.name,
            head.number
        );
        let fetched = pull_head(repo, head).await?;
        git::detached::move_to(&workspace.path, &fetched.at.commit).await?;
        self.record_starting_commit(&workspace.id, fetched.at.commit)
            .await
    }

    /// Move the workspace `id` of a job, before the job's first step, to
    /// `commit`, fetched from `origin` under `reference` when given. The
    /// lifecycle reservation excludes removal meanwhile.
    pub(crate) async fn move_job_workspace(
        &self,
        id: &str,
        commit: &str,
        reference: Option<&str>,
    ) -> Result<()> {
        let workspace = self.workspace(id).await?;
        let (repo, _guard) = self.lock_repository(&workspace.repository_id).await?;
        let reserved = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Updating)
            .await?;
        let result = self
            .move_reserved_job_workspace(&repo, &workspace, commit, reference)
            .await;
        self.set_state(&workspace.id, reserved, workspace.error.clone())
            .await?;
        result
    }

    /// Move the reserved job `workspace`, once its worktree is proven its
    /// own, to `commit`.
    async fn move_reserved_job_workspace(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        commit: &str,
        reference: Option<&str>,
    ) -> Result<()> {
        self.verify_worktree(workspace).await?;
        fetch_job_commit(repo, commit, reference).await?;
        git::detached::move_to(&workspace.path, commit).await?;
        self.record_starting_commit(&workspace.id, commit.to_owned())
            .await
    }

    /// Record `commit` as where the workspace `id` started, which is also
    /// what its diff compares with.
    async fn record_starting_commit(&self, id: &str, commit: String) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET start_commit=?2,base_commit=?2 WHERE id=?1",
                    rusqlite::params![id, commit],
                )?;
                Ok(())
            })
            .await
    }

    /// Record `reference` as what the workspace `id`'s diff compares with,
    /// such as the base branch a PR now targets.
    async fn record_diff_ref(&self, id: &str, reference: String) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET base_ref=?2 WHERE id=?1",
                    rusqlite::params![id, reference],
                )?;
                Ok(())
            })
            .await
    }

    /// Open the removed workspace at a commit `removed` again, without a
    /// branch, at its removed HEAD with `files` from its checkpoint; it keeps
    /// the commit it started `at`.
    pub(super) async fn restore_commit_workspace(
        &self,
        removed: &RemovedWorkspace,
        at: StartingCommit,
        files: Option<RestoredFiles>,
    ) -> Result<Workspace> {
        let (repo, _guard) = self.lock_repository(removed.repository_id.as_str()).await?;
        let diff_ref = surviving_ref(&repo, removed.base_ref.as_deref()).await?;
        let workspace = self
            .record_commit_workspace(
                &repo,
                &removed.name,
                at,
                diff_ref,
                None,
                CreationOptions::default(),
            )
            .await?;
        let head = &removed.checkpoint.head;
        let result = self
            .materialize_commit_worktree(&repo, &workspace, head, files.as_ref())
            .await;
        self.settle_creation(&workspace, result).await
    }

    /// Record a new workspace at a commit named after `name`, which started
    /// `at`, below `parent`, whose diff compares with `diff_ref`, else with
    /// `at`. The caller holds the repository's Git gate.
    async fn record_commit_workspace(
        &self,
        repo: &Repository,
        name: &str,
        at: StartingCommit,
        diff_ref: Option<String>,
        parent: Option<String>,
        options: CreationOptions,
    ) -> Result<Workspace> {
        let name = self.free_workspace_name(name).await?;
        let path = self.new_workspace_path(repo, &name, None).await?;
        let workspace = Workspace {
            base_commit: Some(at.commit.clone()),
            base_ref: diff_ref,
            at: Some(at),
            setup_skipped: options.skip_setup,
            disposable: options.disposable,
            ..Workspace::new_record(repo.id.clone(), name, path, None, WorkspaceState::Preparing)
        };
        self.insert_workspace(workspace, None, parent).await
    }

    /// Check out `head` and configure the worktree with `files` written into
    /// it, returning whether setup is still needed.
    async fn materialize_commit_worktree(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        head: &str,
        files: Option<&RestoredFiles>,
    ) -> Result<bool> {
        git::detached::add_workspace(&repo.path, &workspace.path, head).await?;
        self.configure_worktree(repo, workspace, files, None).await
    }

    /// Resolve `start` in `repo`: the commit, what it was made for and the
    /// name it gives the workspace.
    async fn resolve_start(&self, repo: &Repository, start: CommitStart) -> Result<Start> {
        match start {
            CommitStart::Revision(base) => self.resolve_revision(repo, base.as_deref()).await,
            CommitStart::PullHead(head) => pull_head(repo, &head).await,
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

/// `reference` while `repo` still has it; a deleted one, such as a merged
/// PR's base branch, leaves the diff comparing with the starting commit.
async fn surviving_ref(repo: &Repository, reference: Option<&str>) -> Result<Option<String>> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    Ok(
        git::ref_exists(&repo.path, reference, git::isolated_command)
            .await?
            .then(|| reference.to_owned()),
    )
}

/// Fetch `commit` into `repo` unless it has it: with the ref `reference`
/// `origin` serves it under, which may have moved past it since, and else
/// by itself.
async fn fetch_job_commit(repo: &Repository, commit: &str, reference: Option<&str>) -> Result<()> {
    let present = async || {
        git::resolve_commit(
            &repo.path,
            &format!("{commit}^{{commit}}"),
            git::run_isolated,
        )
        .await
        .is_ok()
    };
    if present().await {
        return Ok(());
    }
    if let Some(reference) = reference {
        git::fetch::reference(&repo.path, "origin", reference).await?;
        if present().await {
            return Ok(());
        }
    }
    git::fetch::reference(&repo.path, "origin", commit)
        .await
        .with_context(|| format!("the job's commit {commit} is not on origin"))?;
    Ok(())
}

/// A PR's `head`, fetched from `origin` and checked to be the commit the
/// forge reported.
async fn pull_head(repo: &Repository, head: &PullHead) -> Result<Start> {
    let fetched = git::fetch::reference(&repo.path, "origin", &head.reference).await?;
    anyhow::ensure!(
        fetched == head.commit,
        "PR #{} moved while its head was fetched; run the command again",
        head.number
    );
    Ok(Start {
        at: StartingCommit {
            commit: fetched,
            made_for: Some(MadeFor::Pr(head.number)),
        },
        name: format!("pr-{}", head.number),
        diff_ref: head.base.clone(),
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
        diff_ref: None,
    }
}

#[cfg(test)]
mod tests;
