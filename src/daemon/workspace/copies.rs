//! Copies: workspaces that start from another workspace's current files or
//! from one of its checkpoints, and record what they started with in a
//! starting checkpoint of their own.
use std::path::PathBuf;

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Manager, NewWorkspace, restore::RestoredFiles};
use crate::client::space_names::SpaceSelector;
use crate::{client::space_names::SpaceName, model::SpaceKind};
use crate::{
    git::{self, worktrunk},
    model::{
        Checkpoint, CheckpointCause, CopiedFrom, Repository, StartingCommit, Workspace,
        WorkspaceRef,
    },
    protocol::CopySource,
    state::WorkspaceState,
};

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
struct Source {
    workspace_id: String,
    repository_id: String,
    name: String,
    branch: Option<String>,
    base_ref: Option<String>,
    base_commit: Option<String>,
    /// The branch of the workspace it builds on, which the copy builds on
    /// too.
    base_branch: Option<String>,
    head: String,
    files: SourceFiles,
}

enum SourceFiles {
    /// The live worktree's current files, ignored ones included.
    Worktree(PathBuf),
    /// A checkpoint's, by its number and the commit that keeps its files.
    Checkpoint { number: u64, commit: String },
}

impl Source {
    /// The name a copy takes when none is given: the copied branch's, or
    /// for a workspace at a commit its name's, with `-copy` added.
    fn copy_name(&self) -> String {
        format!("{}-copy", self.branch.as_deref().unwrap_or(&self.name))
    }

    /// The copied workspace's checkpoint the copy starts from.
    fn checkpoint(&self) -> Option<u64> {
        match self.files {
            SourceFiles::Worktree(_) => None,
            SourceFiles::Checkpoint { number, .. } => Some(number),
        }
    }
}

impl Manager {
    /// Create a copy of `source` in `repository` where `place` says, below
    /// the space `new` names, and take its starting checkpoint. It runs
    /// setup like any workspace unless the caller skips it.
    pub(crate) async fn create_copy(
        &self,
        repository: crate::repository::Selector,
        source: &CopySource,
        place: CopyPlace,
        new: NewWorkspace<'_>,
    ) -> Result<Workspace> {
        self.ensure_parent_repository(&repository, new.parent.as_deref())
            .await?;
        let (repo, _guard) = self.lock_repository(repository).await?;
        let source = self.copy_source(source).await?;
        ensure!(
            source.repository_id == repo.id,
            "workspace {} belongs to another repository",
            source.name
        );
        let workspace = self.record_copy(&repo, &source, place, &new).await?;
        let result = self
            .materialize_copy_worktree(&repo, &workspace, &source, new.git_profile)
            .await;
        self.settle_creation(&workspace, result).await
    }

    /// The workspace `request` names, live, or removed when it names a
    /// checkpoint, as a copy starts from it.
    async fn copy_source(&self, request: &CopySource) -> Result<Source> {
        match self.workspace(&request.workspace).await {
            Ok(workspace) => match request.checkpoint {
                Some(number) => self.live_checkpoint_source(workspace, number).await,
                None => self.files_source(workspace).await,
            },
            Err(error) => match request.checkpoint {
                Some(number) => self.removed_source(&request.workspace, number).await,
                None => Err(error),
            },
        }
    }

    /// The current files of `workspace`, which must be ready, verified and
    /// in no Git operation, and hold nothing a checkpoint cannot keep.
    async fn files_source(&self, workspace: Workspace) -> Result<Source> {
        let name = &workspace.name;
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace {name} is {}; copy it once it is ready",
            workspace.state
        );
        self.verify_worktree(&workspace).await?;
        let path = &workspace.path;
        ensure!(
            !git::status::operation_in_progress(path).await?,
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
        let head = git::resolve_commit(path, "HEAD", git::run_isolated).await?;
        let files = SourceFiles::Worktree(workspace.path.clone());
        Ok(live_source(workspace, head, files))
    }

    /// Checkpoint `number` of the existing `workspace`.
    async fn live_checkpoint_source(&self, workspace: Workspace, number: u64) -> Result<Source> {
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
    async fn removed_source(&self, selector: &SpaceSelector, number: u64) -> Result<Source> {
        let removed = self.claim_removed_workspace(selector).await?;
        let checkpoint = self
            .copied_checkpoint(&removed.workspace_id, &removed.name, number)
            .await?;
        let base_branch = (removed.base_ref.as_deref())
            .and_then(git::strip_local)
            .map(str::to_owned);
        Ok(Source {
            workspace_id: removed.workspace_id,
            repository_id: removed.repository_id,
            name: removed.name.into(),
            branch: checkpoint.branch,
            base_ref: removed.base_ref,
            base_commit: removed.base_commit,
            base_branch,
            head: checkpoint.head,
            files: SourceFiles::Checkpoint {
                number,
                commit: checkpoint.commit,
            },
        })
    }

    /// Checkpoint `number` of workspace `id`, named `name`, refused when it
    /// could not keep the unsaved files.
    async fn copied_checkpoint(&self, id: &str, name: &str, number: u64) -> Result<Checkpoint> {
        let id = id.to_owned();
        let checkpoint = self
            .store
            .run(move |db| super::checkpoints::find(db, &id, number))
            .await?;
        let Some(checkpoint) = checkpoint else {
            anyhow::bail!(
                "workspace {name} has no checkpoint {number}; shoal work restore {name} lists them"
            );
        };
        ensure!(
            !checkpoint.files_omitted,
            "checkpoint {number} of {name} could not keep its unsaved files, so it cannot start a copy"
        );
        Ok(checkpoint)
    }

    /// Record the copy of `source` where `place` says, with the source's
    /// diff base. The caller holds the repository's Git gate.
    async fn record_copy(
        &self,
        repo: &Repository,
        source: &Source,
        place: CopyPlace,
        new: &NewWorkspace<'_>,
    ) -> Result<Workspace> {
        let (name, branch) = match place {
            CopyPlace::Branch(Some(branch)) => {
                git::check_branch_name(Some(&repo.path), &branch).await?;
                let name = SpaceName::derive(SpaceKind::Workspace, &branch);
                (name, Some(self.available_branch(repo, &branch).await?))
            }
            CopyPlace::Branch(None) => {
                let branch = self.free_copy_branch(repo, &source.copy_name()).await?;
                (
                    SpaceName::derive(SpaceKind::Workspace, &branch),
                    Some(branch),
                )
            }
            CopyPlace::Commit => {
                let name = self.free_workspace_name(&source.copy_name()).await?;
                (name, None)
            }
        };
        let path = self
            .new_workspace_path(repo, &name, new.path.clone())
            .await?;
        let at = branch.is_none().then(|| StartingCommit {
            commit: source.head.clone(),
            made_for: None,
        });
        let record = Workspace {
            base_commit: (source.base_commit.clone()).or_else(|| Some(source.head.clone())),
            base_ref: source.base_ref.clone(),
            at,
            setup_skipped: new.options.skip_setup,
            disposable: new.options.disposable,
            ..Workspace::new_record(
                repo.id.clone(),
                name,
                path,
                branch.clone(),
                WorkspaceState::Preparing,
            )
        };
        let base_branch = branch.and(source.base_branch.clone());
        self.insert_workspace(record, base_branch, new.parent.clone())
            .await
    }

    /// The free branch for `name`, or for the first of `name-2`, `name-3`,
    /// ... whose free branch also derives a free workspace name, so a copy
    /// named after its source keeps one name for both.
    async fn free_copy_branch(&self, repo: &Repository, name: &str) -> Result<String> {
        git::check_branch_name(Some(&repo.path), name).await?;
        let mut suffix = 1;
        loop {
            let candidate = match suffix {
                1 => name.to_owned(),
                _ => format!("{name}-{suffix}"),
            };
            // The branch may take its suffix on another component than the
            // last, when that one names a branch too.
            let branch = self.available_branch(repo, &candidate).await?;
            let derived = SpaceName::derive(SpaceKind::Workspace, &branch);
            if self.free_workspace_name(&derived).await? == derived {
                return Ok(branch);
            }
            suffix += 1;
        }
    }

    /// Create the copy's worktree with its source's files, then take its
    /// starting checkpoint, returning whether setup is still needed.
    async fn materialize_copy_worktree(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        source: &Source,
        git_profile: Option<&str>,
    ) -> Result<bool> {
        let needs_setup = match &source.files {
            SourceFiles::Worktree(path) => {
                self.clone_source(repo, workspace, path, &source.head, git_profile)
                    .await?
            }
            SourceFiles::Checkpoint { commit, .. } => {
                let files = RestoredFiles {
                    head: source.head.clone(),
                    snapshot: commit.clone(),
                };
                self.check_out_copy(repo, workspace, &files, git_profile)
                    .await?
            }
        };
        let starting = self
            .checkpoint(workspace, CheckpointCause::Start, None)
            .await?;
        let index = git::checkpoint::index_tree(&workspace.path).await?;
        self.record_copied_from(&workspace.id, source, starting.number, index)
            .await?;
        Ok(needs_setup)
    }

    /// Give the new worktree the index, worktree config and files of the
    /// live worktree at `source` at `head`, then `git_profile` when given,
    /// returning whether setup is still needed. The source's ignored files
    /// take the place of the seed build and `setup.copy_ignored`.
    async fn clone_source(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        source: &std::path::Path,
        head: &str,
        git_profile: Option<&str>,
    ) -> Result<bool> {
        let path = &workspace.path;
        git::copy::add_worktree(&repo.path, path, workspace.branch.as_deref(), head).await?;
        // Recorded before the files are cloned, so a restart can verify and
        // remove the worktree.
        self.record_worktree_identity(workspace).await?;
        git::copy::fill(source, path, head).await?;
        let config = self.config();
        if let Some(name) = git_profile {
            crate::git_profile::apply(path, config.machine.git_profiles.profile(name)?).await?;
        }
        let settings = self.workspace_settings_from(&config, workspace).await?;
        Ok(settings.setup.command.is_some() || settings.hooks.pre_setup.is_some())
    }

    /// Check out the checkpoint's HEAD and write its `files` into the new
    /// worktree, configured as any new workspace's, returning whether setup
    /// is still needed.
    async fn check_out_copy(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        files: &RestoredFiles,
        git_profile: Option<&str>,
    ) -> Result<bool> {
        match &workspace.branch {
            Some(branch) => {
                let config = self.paths.worktrunk_config();
                let head = Some(files.head.as_str());
                worktrunk::create(&repo.path, &config, &workspace.path, branch, head).await?
            }
            None => git::detached::add_workspace(&repo.path, &workspace.path, &files.head).await?,
        }
        self.configure_worktree(repo, workspace, Some(files), git_profile)
            .await
    }

    /// Record what the copy `id` was made from, its starting checkpoint and
    /// the tree of the index it started with.
    async fn record_copied_from(
        &self,
        id: &str,
        source: &Source,
        starting: u64,
        index: String,
    ) -> Result<()> {
        let (id, source_id) = (id.to_owned(), source.workspace_id.clone());
        let checkpoint = source.checkpoint().map(|number| number as i64);
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET copied_from=?2,copied_from_checkpoint=?3,
                        starting_checkpoint=?4,starting_index=?5 WHERE id=?1",
                    params![id, source_id, checkpoint, starting as i64, index],
                )?;
                Ok(())
            })
            .await
    }
}

/// `workspace` as a copy starts from it at `head` with `files`.
fn live_source(workspace: Workspace, head: String, files: SourceFiles) -> Source {
    Source {
        workspace_id: workspace.id,
        repository_id: workspace.repository_id,
        name: workspace.name.into(),
        branch: workspace.branch,
        base_ref: workspace.base_ref,
        base_commit: workspace.base_commit,
        base_branch: workspace.base_workspace.and_then(|base| base.branch),
        head,
        files,
    }
}

/// Fill in what the workspace was copied from, when it is a copy.
pub(super) fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.copied_from = db
        .query_row(
            "SELECT source.id, source.name, source.branch, copy.copied_from_checkpoint,
                copy.starting_checkpoint, json_extract(start.record, '$.head'),
                json_extract(start.record, '$.commit'), copy.starting_index
             FROM workspaces copy
             JOIN checkpoints start
                ON start.workspace_id=copy.id AND start.number=copy.starting_checkpoint
             LEFT JOIN workspaces source ON source.id=copy.copied_from
             WHERE copy.id=?1",
            [&workspace.id],
            |row| {
                let workspace = match row.get::<_, Option<String>>(0)? {
                    Some(id) => Some(WorkspaceRef {
                        id,
                        name: row.get(1)?,
                        branch: row.get(2)?,
                    }),
                    None => None,
                };
                Ok(CopiedFrom {
                    workspace,
                    checkpoint: row.get::<_, Option<i64>>(3)?.map(|number| number as u64),
                    starting_checkpoint: row.get::<_, i64>(4)? as u64,
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
