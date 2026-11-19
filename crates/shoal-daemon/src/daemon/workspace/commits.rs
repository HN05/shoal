//! Workspaces at a commit: worktrees at a detached HEAD with no branch of
//! their own, created from a revision or a PR's head. Removal goes the way
//! of any workspace's.
use crate::forge::numbers::PullNumber;
use anyhow::{Context, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::CommitId;

use super::super::{
    manager::Manager,
    restore::RestoredFiles,
    workspace::{
        NewWorkspace,
        creation::{Configured, PendingWorktree},
    },
};
use crate::{
    daemon::scope::Caller,
    git,
    model::{
        Head, MadeFor, OpenedWorkspace, RemovedSpace, Repository, SpaceKind, StartingCommit,
        Workspace,
    },
    protocol::{CommitStart, CreationOptions, PullHead},
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;
use tokio::sync::OwnedMutexGuard;

/// The commit a workspace at a commit starts at, what it was made for, and
/// the directory and name it derives from them.
pub(super) struct Start {
    pub(super) at: StartingCommit,
    directory: String,
    name: SpaceName,
    /// The ref its diff compares with; none compares with `at`.
    diff_ref: Option<String>,
}

/// The workspace of a CI job, recorded before its worktree is checked out
/// at `commit`, with its repository's Git gate.
pub(crate) struct RecordedJobWorkspace {
    repo: Repository,
    workspace: Workspace,
    commit: CommitId,
    git: OwnedMutexGuard<()>,
}

impl RecordedJobWorkspace {
    pub(crate) fn id(&self) -> &SpaceId {
        &self.workspace.id
    }
}

/// The ID a new workspace's record takes, a new one unless a restore keeps
/// its own, the space it goes below, or none at the top, and the worktree
/// its preparation is still to create, if any.
#[derive(Default)]
pub(in crate::daemon) struct NewRecord {
    pub id: Option<SpaceId>,
    pub parent: Option<SpaceId>,
    pub pending: Option<PendingWorktree>,
}

impl Manager {
    /// Create a workspace at the commit `start` names in `repository` for
    /// `caller`: a child of the space `parent`, by ID, or without one a
    /// top-level workspace of the user's, with the priority its options
    /// ask for, at most the caller's own. It runs setup like any workspace
    /// unless the caller skips it.
    pub async fn create_commit_workspace(
        &self,
        repository: crate::repository::Selector,
        (parent, caller): (Option<SpaceId>, Option<&Caller>),
        start: CommitStart,
        options: CreationOptions,
    ) -> Result<Workspace> {
        let priority = self.given_priority(caller, options.priority).await?;
        let new = NewWorkspace {
            options,
            parent,
            ..Default::default()
        };
        let workspace = self.create_at_commit_start(repository, start, new).await?;
        self.give_priority(&workspace.id, priority).await?;
        Ok(workspace)
    }

    /// Create the workspace `new` describes at the commit `start` names in
    /// `repository`.
    async fn create_at_commit_start(
        &self,
        repository: crate::repository::Selector,
        start: CommitStart,
        new: NewWorkspace<'_>,
    ) -> Result<Workspace> {
        if let CommitStart::Copy(source) = start {
            return (self.create_copy(repository, &source, super::CopyPlace::Commit, new)).await;
        }
        // Checked before resolving the start, which may fetch: a scoped
        // process reaches only its own repository.
        self.ensure_parent_repository(&repository, new.parent.as_ref())
            .await?;
        let (repo, git) = self.lock_repository(repository).await?;
        self.create_at_start(&repo, start, new, git).await
    }

    /// Open PR `url`'s `head` in a workspace at a commit in `repository`:
    /// the one made for or linked to the PR, moved to `head` when the PR's
    /// head moved, so there is at most one per PR, or else a new one as
    /// `new` says, below the space it names or a top-level one of the
    /// user's. A scoped `caller` takes over no workspace outside its scope.
    pub(crate) async fn open_pull_head(
        &self,
        repository: crate::repository::Selector,
        url: crate::forge::items::reference::ItemRef,
        head: PullHead,
        new: NewWorkspace<'_>,
        caller: Option<&Caller>,
    ) -> Result<OpenedWorkspace> {
        self.ensure_parent_repository(&repository, new.parent.as_ref())
            .await?;
        let (repo, git) = self.lock_repository(repository).await?;
        if let Some(workspace) = self.pull_workspace(&repo.id, &url, head.number).await? {
            (self.ensure_takes_over(&workspace, caller)).await?;
            self.move_to_pull_head(&repo, &workspace, &head).await?;
            if let Some(base) = head.base {
                self.record_diff_ref(&workspace.id, base).await?;
            }
            // Its last opener owns it, as a reopened branch's does; a
            // workspace's child stays its parent's.
            let workspace = match workspace.parent {
                Some(parent) if parent.kind() == SpaceKind::Workspace => {
                    self.workspace_by_id(&workspace.id).await?
                }
                _ => {
                    self.give_to_parent(&SpaceSelector::id(&workspace.id), new.parent)
                        .await?
                }
            };
            return Ok(OpenedWorkspace {
                workspace,
                reused: true,
            });
        }
        let start = CommitStart::PullHead(head);
        Ok(OpenedWorkspace {
            workspace: (self.create_at_start(&repo, start, new, git)).await?,
            reused: false,
        })
    }

    /// Create a workspace at `start` in `repo` under its Git gate `git`, as
    /// [`Self::create_commit_workspace`] does, below the space `new` names;
    /// a pending one's worktree waits in its record for its preparation.
    async fn create_at_start(
        &self,
        repo: &Repository,
        start: CommitStart,
        NewWorkspace {
            parent,
            options,
            pending,
            ..
        }: NewWorkspace<'_>,
        git: OwnedMutexGuard<()>,
    ) -> Result<Workspace> {
        // Recorded below its parent, a workspace or a chat, with its
        // record, so one whose creation fails is still there.
        let start = self.resolve_start(repo, start).await?;
        let head = start.at.commit.clone();
        let name = match &options.name {
            Some(given) => given.checked()?,
            None => start.name,
        };
        let new = NewRecord {
            id: None,
            parent,
            pending: pending.then(|| PendingWorktree::commit(head.clone())),
        };
        let workspace = self
            .record_commit_workspace(
                repo,
                (&start.directory, name),
                start.at,
                start.diff_ref,
                new,
                options,
            )
            .await?;
        if pending {
            return self.workspace_by_id(&workspace.id).await;
        }
        let result = self
            .materialize_commit_worktree(repo, &workspace, &head, None)
            .await;
        self.settle_creation(repo, &workspace, result, git).await
    }

    /// Record the workspace a CI job runs in, in `directory` and named
    /// after the job: at `commit`, fetched
    /// from the ref `reference` `origin` serves it under, or else by
    /// itself, made for nothing, so no PR reuses it, and disposable, without
    /// setup. [`Self::check_out_job_workspace`] creates its worktree.
    pub(crate) async fn record_job_workspace(
        &self,
        repository_id: &RepositoryId,
        (directory, name): (&str, SpaceName),
        commit: &CommitId,
        reference: Option<&str>,
    ) -> Result<RecordedJobWorkspace> {
        let (repo, git) = self.lock_repository(repository_id).await?;
        fetch_job_commit(&repo, commit, reference).await?;
        let at = StartingCommit {
            commit: commit.to_owned(),
            made_for: None,
        };
        let options = CreationOptions {
            name: None,
            skip_setup: true,
            disposable: true,
            ..Default::default()
        };
        let workspace = self
            .record_commit_workspace(
                &repo,
                (directory, name),
                at,
                None,
                NewRecord::default(),
                options,
            )
            .await?;
        Ok(RecordedJobWorkspace {
            repo,
            workspace,
            commit: commit.to_owned(),
            git,
        })
    }

    /// Check out the worktree of the `recorded` job workspace at its commit;
    /// the seed clone still happens. One that fails stays `failed`.
    pub(crate) async fn check_out_job_workspace(
        &self,
        recorded: RecordedJobWorkspace,
    ) -> Result<Workspace> {
        let RecordedJobWorkspace {
            repo,
            workspace,
            commit,
            git,
        } = recorded;
        let result = self
            .materialize_commit_worktree(&repo, &workspace, &commit, None)
            .await;
        self.settle_creation(&repo, &workspace, result, git).await
    }

    /// The workspace at a commit of `repository_id` made for PR `number` or
    /// linked to it at `url`.
    async fn pull_workspace(
        &self,
        repository_id: &RepositoryId,
        url: &crate::forge::items::reference::ItemRef,
        number: PullNumber,
    ) -> Result<Option<Workspace>> {
        let made_for = Some(MadeFor::Pr(number));
        Ok(self.list_workspaces().await?.into_iter().find(|workspace| {
            workspace.repository() == repository_id
                && workspace.at().is_some_and(|at| {
                    at.made_for == made_for || workspace.links.prs.iter().any(|pr| pr.url == *url)
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
            .at()
            .context("workspace records no starting commit")?;
        if at.commit == head.commit {
            return Ok(());
        }
        let reserved = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Updating)
            .await?;
        let result = self
            .replace_pull_head(repo, workspace, at.commit.as_str(), head)
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
        self.move_to_start(workspace, fetched.at.commit).await
    }

    /// Move the workspace `id` of a job, before the job's first step, to
    /// `commit`, fetched from `origin` under `reference` when given, unless
    /// its checkout is there already. The
    /// lifecycle reservation excludes removal meanwhile.
    pub(crate) async fn move_job_workspace(
        &self,
        id: &SpaceId,
        commit: &CommitId,
        reference: Option<&str>,
    ) -> Result<()> {
        let workspace = self.workspace_by_id(id).await?;
        let (repo, _guard) = self.lock_repository(workspace.repository()).await?;
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
    /// own, to `commit`, unless its checkout is there already.
    async fn move_reserved_job_workspace(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        commit: &CommitId,
        reference: Option<&str>,
    ) -> Result<()> {
        self.verify_worktree(workspace).await?;
        if crate::daemon::links::pr::current_head(workspace).await? == *commit {
            return Ok(());
        }
        fetch_job_commit(repo, commit, reference).await?;
        self.move_to_start(workspace, commit.to_owned()).await
    }

    /// Check out `commit` in the reserved workspace at a commit, whose
    /// repository's Git gate the caller holds, and record it as where the
    /// workspace starts.
    pub(super) async fn move_to_start(
        &self,
        workspace: &Workspace,
        commit: CommitId,
    ) -> Result<()> {
        git::detached::move_to(&workspace.path, commit.as_str()).await?;
        self.record_starting_commit(&workspace.id, commit).await
    }

    /// Record `commit` as where the workspace `id` started, which is also
    /// what its diff compares with.
    async fn record_starting_commit(&self, id: &SpaceId, commit: CommitId) -> Result<()> {
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
    async fn record_diff_ref(&self, id: &SpaceId, reference: String) -> Result<()> {
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
    pub(in crate::daemon) async fn restore_commit_workspace(
        &self,
        removed: &RemovedSpace,
        new: NewRecord,
        at: StartingCommit,
        files: Option<RestoredFiles>,
    ) -> Result<Workspace> {
        let (repo, git) = self.lock_repository(removed.repository()?).await?;
        let diff_ref = surviving_ref(&repo, removed.kept_workspace()?.base_ref.as_deref()).await?;
        // Its directory comes from where it started, never its name.
        let directory = at.directory_seed();
        let workspace = self
            .record_commit_workspace(
                &repo,
                (&directory, removed.name.clone()),
                at,
                diff_ref,
                new,
                CreationOptions {
                    skip_setup: removed.kept_workspace()?.setup_skipped,
                    ..Default::default()
                },
            )
            .await?;
        let head = removed.kept_workspace()?.checkpoint.head.as_str();
        let result = self
            .materialize_commit_worktree(&repo, &workspace, &head.into(), files.as_ref())
            .await;
        self.settle_creation(&repo, &workspace, result, git).await
    }

    /// Record a new workspace at a commit in a directory named after
    /// `directory`, with `name`, which started `at`, as `new` places it,
    /// whose diff compares with `diff_ref`, else with `at`. The caller holds
    /// the repository's Git gate.
    async fn record_commit_workspace(
        &self,
        repo: &Repository,
        (directory, name): (&str, SpaceName),
        at: StartingCommit,
        diff_ref: Option<String>,
        new: NewRecord,
        options: CreationOptions,
    ) -> Result<Workspace> {
        let path = self.new_workspace_path(repo, directory, None).await?;
        let id = new.id.unwrap_or_else(SpaceId::generate);
        let state = WorkspaceState::Preparing;
        let base_commit = Some(at.commit.clone());
        let head = Head::Commit(at);
        let mut workspace = Workspace::record(id, repo.id.clone(), name, path, head, state);
        workspace.base_commit = base_commit;
        workspace.base_ref = diff_ref;
        workspace.setup_skipped = options.skip_setup;
        workspace.disposable = options.disposable;
        self.insert_workspace(workspace, None, new.parent, new.pending, true)
            .await
    }

    /// Check out `head` and configure the worktree with `files` written into
    /// it.
    pub(super) async fn materialize_commit_worktree(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        head: &CommitId,
        files: Option<&RestoredFiles>,
    ) -> Result<Configured> {
        git::detached::add_workspace(&repo.path, &workspace.path, head.as_str()).await?;
        self.configure_worktree(workspace, files, None).await
    }

    /// Resolve `start` in `repo`: the commit, what it was made for and the
    /// name it gives the workspace.
    async fn resolve_start(&self, repo: &Repository, start: CommitStart) -> Result<Start> {
        match start {
            CommitStart::Revision(base) => self.resolve_revision(repo, base.as_deref()).await,
            CommitStart::PullHead(head) => pull_head(repo, &head).await,
            CommitStart::Copy(_) => anyhow::bail!("a copy starts from its source's HEAD"),
        }
    }

    /// Resolve a revision as a branch workspace's `--base` resolves, fetching
    /// a remote branch it names; a tag or commit that does not resolve makes
    /// `origin` fetch with its tags once before trying again.
    pub(super) async fn resolve_revision(
        &self,
        repo: &Repository,
        base: Option<&str>,
    ) -> Result<Start> {
        let revision = self.new_branch_base(repo, base).await?;
        let resolve = || git::resolve_named_commit(&repo.path, &revision, git::run_isolated);
        let (commit, reference) = match resolve().await {
            Ok(resolved) => resolved,
            Err(error) => {
                if git::fetch::with_tags(&repo.path, "origin").await.is_err() {
                    return Err(error.context(format!("{revision} is not a commit here")));
                }
                (resolve().await)
                    .with_context(|| format!("{revision} is not a commit here or on origin"))?
            }
        };
        Ok(named_start(commit, base, &reference))
    }
}

/// `reference` while `repo` still has it; a deleted one, such as a merged
/// PR's base branch, leaves the diff comparing with the starting commit.
async fn surviving_ref(repo: &Repository, reference: Option<&str>) -> Result<Option<String>> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    Ok(git::ref_exists(&repo.path, reference)
        .await?
        .then(|| reference.to_owned()))
}

/// Fetch `commit` into `repo` unless it has it: with the ref `reference`
/// `origin` serves it under, which may have moved past it since, and else
/// by itself.
async fn fetch_job_commit(
    repo: &Repository,
    commit: &CommitId,
    reference: Option<&str>,
) -> Result<()> {
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
    git::fetch::reference(&repo.path, "origin", commit.as_str())
        .await
        .with_context(|| format!("the job's commit {commit} is not on origin"))?;
    Ok(())
}

/// A PR's `head`, fetched from `origin` and checked to be the commit the
/// forge reported.
pub(super) async fn pull_head(repo: &Repository, head: &PullHead) -> Result<Start> {
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
        directory: format!("pr-{}", head.number),
        name: SpaceName::derive(&format!("pr-{}", head.number), "workspace"),
        diff_ref: head.base.clone(),
    })
}

/// What a revision that resolved to `commit` names: a tag by its name, a
/// branch as `base` gave it, or the default branch when `base` named none,
/// with the workspace named after the tag, or the branch at the short
/// commit, such as `main at 84d4405`, in a directory such as
/// `main-84d4405`; a bare commit is made for nothing and named by its short
/// commit.
fn named_start(commit: CommitId, base: Option<&str>, reference: &str) -> Start {
    let short = commit.abbreviated(7).to_owned();
    let branch = reference.strip_prefix(git::LOCAL_REFS).or_else(|| {
        reference
            .strip_prefix("refs/remotes/")
            .and_then(|remote| remote.split_once('/'))
            .map(|(_, branch)| branch)
    });
    let (directory, name, made_for) = match (reference.strip_prefix("refs/tags/"), branch) {
        (Some(tag), _) => (tag.to_owned(), tag.to_owned(), Some(tag.to_owned())),
        (None, Some(branch)) => (
            format!("{branch}-{short}"),
            format!("{branch} at {short}"),
            Some(base.unwrap_or(branch).to_owned()),
        ),
        (None, None) => (short.clone(), short, None),
    };
    Start {
        at: StartingCommit {
            commit,
            made_for: made_for.map(MadeFor::Ref),
        },
        directory,
        name: SpaceName::derive(&name, "workspace"),
        diff_ref: None,
    }
}

#[cfg(test)]
mod tests;
