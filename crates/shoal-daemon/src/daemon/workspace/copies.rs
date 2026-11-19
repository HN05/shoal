//! Copies: workspaces that start from another workspace's current files or
//! from one of its checkpoints, and record what they started with in a
//! starting checkpoint of their own.
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_refs::{BranchName, CommitId};

use super::super::{
    manager::Manager,
    restore::RestoredFiles,
    workspace::{
        NewWorkspace,
        creation::{Configured, PendingWorktree, creates_branch},
    },
};
use crate::{
    git,
    model::{
        Checkpoint, CheckpointCause, CopiedFrom, Head, Repository, StartingCommit, Workspace,
        WorkspaceRef,
    },
    protocol::CopySource,
    state::WorkspaceState,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;
use tokio::sync::OwnedMutexGuard;

/// Where a copy goes.
#[derive(Debug)]
pub(crate) enum CopyPlace {
    /// On a new branch, this one or else one named after the copied
    /// workspace's.
    Branch(Option<String>),
    /// At the copied HEAD, without a branch.
    Commit,
}

/// What a copy starts from, resolved from its request: a live or removed
/// workspace's HEAD, diff base and files.
pub(super) struct Source {
    workspace_id: SpaceId,
    repository_id: RepositoryId,
    name: String,
    branch: Option<BranchName>,
    base_ref: Option<String>,
    base_commit: Option<CommitId>,
    /// The branch of the workspace it builds on, which the copy builds on
    /// too.
    base_branch: Option<BranchName>,
    head: CommitId,
    files: SourceFiles,
}

/// When reading a live source checks that its files can be copied: now,
/// or at the creation of a pending copy's worktree, which copies them as
/// they are then.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FilesCheck {
    Now,
    AtCreation,
}

enum SourceFiles {
    /// The live worktree's current files, ignored ones included.
    Worktree(PathBuf),
    /// A checkpoint's, by its number and the commit that keeps its files.
    Checkpoint {
        number: CheckpointNumber,
        commit: CommitId,
    },
}

impl Source {
    /// The name of the workspace copied.
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// The branch a copy takes when none is given: the copied branch's, or
    /// for a workspace at a commit its short HEAD, with `-copy` added;
    /// nothing is built from the copied workspace's name.
    fn copy_name(&self) -> String {
        let short = self.head.abbreviated(7);
        format!(
            "{}-copy",
            self.branch.as_ref().map_or(short, BranchName::as_str)
        )
    }

    /// The copied workspace's checkpoint the copy starts from.
    fn checkpoint(&self) -> Option<CheckpointNumber> {
        match self.files {
            SourceFiles::Worktree(_) => None,
            SourceFiles::Checkpoint { number, .. } => Some(number),
        }
    }
}

impl Manager {
    /// The current files of the live workspace `id`, as a copy starts from
    /// them.
    pub(super) async fn live_copy_source(&self, id: &SpaceId) -> Result<Source> {
        let workspace = self.workspace_by_id(id).await?;
        self.files_source(workspace, FilesCheck::Now).await
    }

    /// Whether `source` still has the HEAD, index and unsaved files the copy
    /// `copied` started with.
    pub(super) async fn source_unchanged(
        &self,
        source: &Source,
        copied: &CopiedFrom,
    ) -> Result<bool> {
        let SourceFiles::Worktree(path) = &source.files else {
            return Ok(false);
        };
        if source.head != copied.head || git::checkpoint::index_tree(path).await? != copied.index {
            return Ok(false);
        }
        let message = format!("Shoal comparison of {} with its copy", source.name);
        let now = git::checkpoint::snapshot(path, &message, true).await?;
        git::checkpoint::same_files(path, copied.commit.as_str(), now.commit.as_str()).await
    }

    /// Copy the live `source`'s files into the reserved copy `workspace`
    /// again, in place, at the source's HEAD, after a checkpoint keeps what
    /// it holds, and record what it now started with. Its branch, if it has
    /// one, moves to that HEAD.
    pub(super) async fn copy_again(&self, workspace: &Workspace, source: &Source) -> Result<()> {
        let SourceFiles::Worktree(path) = &source.files else {
            anyhow::bail!("only a live workspace's files are copied again");
        };
        let kept = self
            .checkpoint(workspace, CheckpointCause::Refresh, None)
            .await?;
        git::copy::refill(path, &workspace.path, &source.head)
            .await
            .with_context(|| {
                format!(
                    "copying {}'s files into {} again failed; shoal restore {} --checkpoint {} returns it to what it held",
                    source.name, workspace.name, workspace.number, kept.number
                )
            })?;
        self.record_copy_checkpoint(workspace, source).await
    }

    /// The name of the removed workspace the copy `id` was made from, while
    /// its removal record lasts.
    pub(super) async fn removed_source_name(&self, id: &SpaceId) -> Result<Option<String>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT removed.name FROM workspaces copy
                         JOIN removed_spaces removed ON removed.space_id=copy.copied_from
                         WHERE copy.id=?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Create a copy of `source` in `repository` where `place` says, below
    /// the space `new` names, and take its starting checkpoint; a pending
    /// one's worktree waits in its record for its preparation, which checks
    /// the files it copies then, as creating one now checks them again. It
    /// runs setup like any workspace unless the caller skips it.
    pub(crate) async fn create_copy(
        &self,
        repository: crate::repository::Selector,
        source: &CopySource,
        place: CopyPlace,
        new: NewWorkspace<'_>,
    ) -> Result<Workspace> {
        self.ensure_parent_repository(&repository, new.parent.as_ref())
            .await?;
        let (repo, git) = self.lock_repository(repository).await?;
        let check = if new.pending {
            FilesCheck::AtCreation
        } else {
            FilesCheck::Now
        };
        let source = self.copy_source(source, check).await?;
        ensure!(
            source.repository_id == repo.id,
            "workspace {} belongs to another repository",
            source.name
        );
        let workspace = self.record_copy(&repo, &source, place, &new).await?;
        if new.pending {
            return self.workspace_by_id(&workspace.id).await;
        }
        self.create_recorded_worktree(&repo, &workspace, git).await
    }

    /// Create the worktree of the recorded copy `workspace` with the files
    /// of the workspace `copied` names as they are now, or give the one an
    /// earlier try created them again, then settle its creation under the
    /// repository's Git gate `git`.
    pub(super) async fn create_pending_copy(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        copied: &CopySource,
        git_profile: Option<&str>,
        git: OwnedMutexGuard<()>,
    ) -> Result<Workspace> {
        let source = match self.copy_source(copied, FilesCheck::Now).await {
            Ok(source) => source,
            Err(error) => return self.record_creation(workspace, Err(error)).await,
        };
        let result = match workspace.git_dir {
            // An earlier try created it, then failed to fill or configure it.
            Some(_) => {
                self.refill_copy_worktree(workspace, &source, git_profile)
                    .await
            }
            None => (self.materialize_copy_worktree(repo, workspace, &source, git_profile)).await,
        };
        self.settle_copy(repo, workspace, &source, result, git)
            .await
    }

    /// Settle the creation of the copy `workspace` of `source`, whose
    /// worktree was `materialized`, as [`Self::settle_creation`] does,
    /// taking its starting checkpoint once the seed build is in.
    async fn settle_copy(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        source: &Source,
        materialized: Result<Configured>,
        git: OwnedMutexGuard<()>,
    ) -> Result<Workspace> {
        let _creation = self.hold_creation(workspace, git).await;
        let seeded = self.seed_worktree(repo, workspace, materialized).await;
        let started = self.record_copy_start(workspace, source, seeded).await;
        self.record_creation(workspace, started).await
    }

    /// The repository of the workspace `request` names, live, or removed
    /// when it names a checkpoint, as [`Self::copy_source`] reads it.
    pub(crate) async fn copied_repository(&self, request: &CopySource) -> Result<RepositoryId> {
        match self.workspace(&request.workspace).await {
            Ok(workspace) => Ok(workspace.repository().clone()),
            Err(error) => match request.checkpoint {
                Some(_) => {
                    let removed = self.removed_space(&request.workspace).await?;
                    Ok(removed.repository()?.clone())
                }
                None => Err(error),
            },
        }
    }

    /// The workspace `request` names, live, or removed when it names a
    /// checkpoint, as a copy starts from it, checking a live worktree's
    /// files when `check` says.
    async fn copy_source(&self, request: &CopySource, check: FilesCheck) -> Result<Source> {
        match self.workspace(&request.workspace).await {
            Ok(workspace) => match request.checkpoint {
                Some(number) => self.live_checkpoint_source(workspace, number).await,
                None => self.files_source(workspace, check).await,
            },
            Err(error) => match request.checkpoint {
                Some(number) => self.removed_source(&request.workspace, number).await,
                None => Err(error),
            },
        }
    }

    /// The current files of `workspace`, which must be ready and verified,
    /// at its HEAD, checked as copyable when `check` says.
    async fn files_source(&self, workspace: Workspace, check: FilesCheck) -> Result<Source> {
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace {} is {}; copy it once it is ready",
            workspace.name,
            workspace.state
        );
        let git_dir = self.verified_git_dir(&workspace).await?;
        if check == FilesCheck::Now {
            ensure_copyable(&workspace, &git_dir).await?;
        }
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        let files = SourceFiles::Worktree(workspace.path.clone());
        Ok(live_source(workspace, head, files))
    }

    /// Checkpoint `number` of the existing `workspace`.
    async fn live_checkpoint_source(
        &self,
        workspace: Workspace,
        number: CheckpointNumber,
    ) -> Result<Source> {
        let checkpoint = self
            .copied_checkpoint(&workspace.id, &workspace.name, number)
            .await?;
        let files = SourceFiles::Checkpoint {
            number,
            commit: checkpoint.commit,
        };
        Ok(live_source(workspace, checkpoint.head, files))
    }

    /// Checkpoint `number` of the removed workspace `selector` names, with
    /// the diff base its removal record keeps.
    async fn removed_source(
        &self,
        selector: &SpaceSelector,
        number: CheckpointNumber,
    ) -> Result<Source> {
        let removed = self.claim_removed_space(selector).await?;
        let checkpoint = self
            .copied_checkpoint(&removed.space_id, &removed.name, number)
            .await?;
        let repository_id = removed.repository()?.clone();
        let kept = removed.kept_workspace()?.clone();
        let base_branch = (kept.base_ref.as_deref())
            .and_then(git::strip_local)
            .map(str::to_owned);
        Ok(Source {
            workspace_id: removed.space_id,
            repository_id,
            name: removed.name.into(),
            branch: checkpoint.branch,
            base_ref: kept.base_ref,
            base_commit: kept.base_commit,
            base_branch: base_branch.map(BranchName::from),
            head: checkpoint.head,
            files: SourceFiles::Checkpoint {
                number,
                commit: checkpoint.commit,
            },
        })
    }

    /// Checkpoint `number` of workspace `id`, named `name`, refused when it
    /// could not keep the unsaved files.
    async fn copied_checkpoint(
        &self,
        id: &SpaceId,
        name: &str,
        number: CheckpointNumber,
    ) -> Result<Checkpoint> {
        let owned = id.to_owned();
        let checkpoint = self
            .store
            .run(move |db| super::checkpoints::find(db, &owned, number))
            .await?;
        let Some(checkpoint) = checkpoint else {
            anyhow::bail!(
                "workspace {name} has no checkpoint {number}; shoal restore {id} lists them"
            );
        };
        ensure!(
            !checkpoint.files_omitted,
            "checkpoint {number} of {name} could not keep its unsaved files, so it cannot start a copy"
        );
        Ok(checkpoint)
    }

    /// Record the copy of `source` where `place` says, with the source's
    /// diff base and how its worktree is created. The caller holds the
    /// repository's Git gate.
    async fn record_copy(
        &self,
        repo: &Repository,
        source: &Source,
        place: CopyPlace,
        new: &NewWorkspace<'_>,
    ) -> Result<Workspace> {
        // Without a name of its own, a copy is named after the branch it
        // asks for.
        let requested = match &place {
            CopyPlace::Branch(Some(branch)) => branch.clone(),
            CopyPlace::Branch(None) | CopyPlace::Commit => source.copy_name(),
        };
        let head = match place {
            CopyPlace::Branch(_) => {
                git::check_branch_name(&requested)?;
                let branch = self.available_branch(repo, &requested).await?;
                ensure!(
                    !new.exact_branch || branch == requested,
                    "branch {requested} conflicts with an existing branch or workspace; name another"
                );
                Head::Branch(branch)
            }
            CopyPlace::Commit => Head::Commit(StartingCommit {
                commit: source.head.clone(),
                made_for: None,
            }),
        };
        let name = match &new.options.name {
            Some(given) => given.checked()?,
            None => SpaceName::derive(&requested, "workspace"),
        };
        let chosen = new.path.is_none();
        let path = self
            // The directory takes its own suffix, never the branch's.
            .new_workspace_path(repo, &requested, new.path.clone())
            .await?;
        let base_branch = head.branch().and(source.base_branch.clone());
        let mut record =
            Workspace::new_record(repo.id.clone(), name, path, head, WorkspaceState::Preparing);
        record.base_commit = (source.base_commit.clone()).or_else(|| Some(source.head.clone()));
        record.base_ref = source.base_ref.clone();
        record.setup_skipped = new.options.skip_setup;
        record.disposable = new.options.disposable;
        let id = source.workspace_id.clone();
        let pending = PendingWorktree::copy(id, source.checkpoint(), new.git_profile);
        let parent = new.parent.clone();
        self.insert_workspace(record, base_branch, parent, Some(pending), chosen)
            .await
    }

    /// Create the copy's worktree with its source's files.
    async fn materialize_copy_worktree(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        source: &Source,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        match &source.files {
            SourceFiles::Worktree(path) => {
                self.clone_source(repo, workspace, path, &source.head, git_profile)
                    .await
            }
            SourceFiles::Checkpoint { commit, .. } => {
                let files = checkpoint_files(&source.head, commit);
                self.check_out_copy(repo, workspace, &files, git_profile)
                    .await
            }
        }
    }

    /// Give the copy's worktree, which an earlier creation of `workspace`
    /// left, its `source`'s files again, and configure it, once it verifies
    /// as the one Git created for it, still on its recorded branch or
    /// detached, and a checkpoint keeps what it holds, refusing a Git
    /// operation under way there and what a checkpoint cannot keep.
    async fn refill_copy_worktree(
        &self,
        workspace: &Workspace,
        source: &Source,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        let branch = workspace.branch().map(BranchName::as_str);
        self.verify_checkout(workspace, branch).await?;
        ensure!(
            !git::status::operation_in_progress(&workspace.path).await?,
            "a Git operation is under way in workspace {}; finish or abort it first",
            workspace.name
        );
        let kept = self
            .checkpoint(workspace, CheckpointCause::Refresh, None)
            .await?;
        ensure!(
            !kept.files_omitted,
            "checkpoint {} could not keep {}'s unsaved files; commit or remove them first",
            kept.number,
            workspace.name
        );
        match &source.files {
            SourceFiles::Worktree(path) => {
                git::copy::refill(path, &workspace.path, &source.head).await?;
                self.configure_clone(workspace, git_profile).await
            }
            SourceFiles::Checkpoint { commit, .. } => {
                rewind_copy(workspace, &checkpoint_files(&source.head, commit)).await?;
                self.configure_worktree(workspace, None, git_profile).await
            }
        }
    }

    /// Take the `seeded` copy's starting checkpoint, which then holds the
    /// seed's outputs as the copy started with them, and record what it was
    /// made from, returning whether setup is still needed.
    async fn record_copy_start(
        &self,
        workspace: &Workspace,
        source: &Source,
        seeded: Result<bool>,
    ) -> Result<bool> {
        let needs_setup = seeded?;
        self.record_copy_checkpoint(workspace, source).await?;
        Ok(needs_setup)
    }

    /// Take the copy `workspace`'s starting checkpoint of what it now holds
    /// and record it with what it was copied from.
    async fn record_copy_checkpoint(&self, workspace: &Workspace, source: &Source) -> Result<()> {
        let starting = self
            .checkpoint(workspace, CheckpointCause::Start, None)
            .await?;
        let index = git::checkpoint::index_tree(&workspace.path).await?;
        self.record_copied_from(&workspace.id, source, starting.number, index)
            .await
    }

    /// Give the new worktree the index, worktree config and files of the
    /// live worktree at `source` at `head`, then `git_profile` when given.
    /// The source's ignored files take the place of the seed build and
    /// `setup.copy_ignored`.
    async fn clone_source(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        source: &std::path::Path,
        head: &CommitId,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        let path = &workspace.path;
        let new_branch = creates_branch(repo, workspace).await?;
        let branch = workspace.branch();
        git::copy::add_worktree(&repo.path, path, branch, head.as_str(), new_branch).await?;
        // Recorded before the files are cloned, so a restart can verify and
        // remove the worktree.
        self.record_worktree_identity(workspace).await?;
        git::copy::fill(source, path, head).await?;
        self.configure_clone(workspace, git_profile).await
    }

    /// Apply `git_profile` to the copy `workspace`, which holds its live
    /// source's files, ignored ones included, so it takes no seed build.
    async fn configure_clone(
        &self,
        workspace: &Workspace,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        let config = self.config();
        if let Some(name) = git_profile {
            let profile = config.machine.git_profiles.profile(name)?;
            crate::git_profile::apply(&workspace.path, profile).await?;
        }
        let settings = self.workspace_settings_from(&config, workspace).await?;
        Ok(Configured {
            needs_setup: settings.setup.command.is_some() || settings.hooks.pre_setup.is_some(),
            seed: None,
        })
    }

    /// Check out the checkpoint's HEAD and write its `files` into the new
    /// worktree, configured as any new workspace's.
    async fn check_out_copy(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        files: &RestoredFiles,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        match workspace.branch() {
            Some(branch) => {
                let new_branch = creates_branch(repo, workspace).await?;
                let head = new_branch.then_some(files.head.as_str());
                git::worktree::add(&repo.path, &workspace.path, branch, head).await?
            }
            None => {
                git::detached::add_workspace(&repo.path, &workspace.path, files.head.as_str())
                    .await?
            }
        }
        self.configure_worktree(workspace, Some(files), git_profile)
            .await
    }

    /// Record what the copy `id` was made from, its starting checkpoint and
    /// the tree of the index it started with, and for a copy at a commit the
    /// source's HEAD as its starting commit.
    async fn record_copied_from(
        &self,
        id: &SpaceId,
        source: &Source,
        starting: CheckpointNumber,
        index: String,
    ) -> Result<()> {
        let (id, source_id) = (id.to_owned(), source.workspace_id.clone());
        let (checkpoint, head) = (source.checkpoint(), source.head.clone());
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET copied_from=?2,copied_from_checkpoint=?3,
                        starting_checkpoint=?4,starting_index=?5,
                        start_commit=iif(start_commit IS NULL, NULL, ?6) WHERE id=?1",
                    params![id, source_id, checkpoint, starting, index, head],
                )?;
                Ok(())
            })
            .await
    }
}

/// The files a copy of a checkpoint writes into its worktree at `head`,
/// which the checkpoint's `commit` keeps.
fn checkpoint_files(head: &CommitId, commit: &CommitId) -> RestoredFiles {
    RestoredFiles {
        head: head.clone(),
        snapshot: commit.clone(),
    }
}

/// Return the copy `workspace` to the checkpoint `files` it copies, as
/// returning to a checkpoint does, refusing ignored files it would
/// overwrite, which no checkpoint keeps.
async fn rewind_copy(workspace: &Workspace, files: &RestoredFiles) -> Result<()> {
    let (path, head, snapshot) = (
        &workspace.path,
        files.head.as_str(),
        files.snapshot.as_str(),
    );
    let ignored = git::checkpoint::ignored_collisions(path, head, snapshot).await?;
    ensure!(
        ignored.is_empty(),
        "copying the checkpoint into {} again would overwrite ignored files no checkpoint keeps; move them first: {}",
        workspace.name,
        ignored.join(", ")
    );
    git::checkpoint::rewind(path, head, snapshot).await
}

/// Refuse copying the files of the verified `workspace`, whose admin
/// directory is `git_dir`, while a Git operation is under way there or it
/// holds what a checkpoint cannot keep or Git metadata a copy would share.
async fn ensure_copyable(workspace: &Workspace, git_dir: &Path) -> Result<()> {
    let (name, path) = (&workspace.name, &workspace.path);
    ensure!(
        !git::status::operation_in_progress_at(path, git_dir).await?,
        "a Git operation is under way in {name}; copy it once it is finished or aborted"
    );
    ensure!(
        git::checkpoint::unsaved_bytes(path).await?.is_some(),
        "{name} has an untracked nested repository or a changed submodule, whose files a copy cannot record"
    );
    let shared = git::copy::shared_metadata(path).await?;
    ensure!(
        shared.is_empty(),
        "{name} holds submodules or worktrees whose Git metadata a copy would share: {}",
        shared.join(", ")
    );
    Ok(())
}

/// `workspace` as a copy starts from it at `head` with `files`.
fn live_source(workspace: Workspace, head: CommitId, files: SourceFiles) -> Source {
    Source {
        repository_id: workspace.repository().clone(),
        workspace_id: workspace.id,
        name: workspace.name.into(),
        branch: workspace.part.branch().cloned(),
        base_ref: workspace.part.base_ref,
        base_commit: workspace.part.base_commit,
        base_branch: (workspace.part.base_workspace.and_then(|base| base.branch)),
        head,
        files,
    }
}

/// Fill in what the workspace was copied from, when it is a copy.
pub(super) fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.copied_from = db
        .query_row(
            "SELECT source.id, source_space.name, source.branch, copy.copied_from_checkpoint,
                copy.starting_checkpoint, json_extract(start.record, '$.head'),
                json_extract(start.record, '$.commit'), copy.starting_index
             FROM workspaces copy
             JOIN checkpoints start
                ON start.workspace_id=copy.id AND start.number=copy.starting_checkpoint
             LEFT JOIN workspaces source ON source.id=copy.copied_from
             LEFT JOIN spaces source_space ON source_space.id=source.id
             WHERE copy.id=?1",
            [&workspace.id],
            |row| {
                let workspace = match row.get::<_, Option<SpaceId>>(0)? {
                    Some(id) => Some(WorkspaceRef {
                        id,
                        name: row.get(1)?,
                        branch: row.get(2)?,
                    }),
                    None => None,
                };
                Ok(CopiedFrom {
                    workspace,
                    checkpoint: row.get(3)?,
                    starting_checkpoint: row.get(4)?,
                    head: row.get(5)?,
                    commit: row.get(6)?,
                    index: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                })
            },
        )
        .optional()?;
    Ok(())
}

#[cfg(test)]
mod tests;
