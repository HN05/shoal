//! Importing a workspace another Shoal exported: recording it as removed
//! here, then restoring it.
use crate::client::space_ids::SpaceId;
use shoal_refs::CommitId;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::client::space_names::SpaceName;
use crate::client::space_names::SpaceSelector;
use crate::daemon::workspace::Manager;
use crate::refusal::{Reason, Refusal};
use crate::{
    git,
    model::{RemovedKind, RemovedSpace, Repository, RestoredSpace, RestoredWorkspace},
    time::UnixTime,
    transfer::CarriedSessionFile,
};

impl Manager {
    /// Open a workspace another Shoal exported: record it as removed here
    /// with its checkpoint, fetched from `bundle` or else on the default
    /// branch, and its agents' `session_files`, and restore it; a `copy` as
    /// a new workspace of its own. A failed restore leaves the record for
    /// `shoal restore`.
    pub async fn import_workspace(
        &self,
        selector: &crate::repository::Selector,
        bundle: Option<&Path>,
        prerequisite: Option<&str>,
        removed: RemovedSpace,
        copy: bool,
        session_files: &[CarriedSessionFile],
    ) -> Result<RestoredWorkspace> {
        let removed = self
            .record_package(selector, bundle, prerequisite, removed, copy, session_files)
            .await
            .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
        self.restore_workspace(&SpaceSelector::id(&removed.space_id))
            .await
            .with_context(|| {
                format!(
                    "the imported workspace is recorded as removed; shoal restore {} retries",
                    removed.space_id
                )
            })
    }

    /// Open a chat another Shoal's package carried: record it as a removed
    /// chat of the repository `selector` names, or without one in
    /// `root_dir`, as its removal here would, and restore it under its ID,
    /// below its parent and in the worktree it ran in where those are here.
    /// A failed restore leaves the record for `shoal restore`.
    pub async fn import_chat(
        &self,
        selector: Option<&crate::repository::Selector>,
        removed: RemovedSpace,
    ) -> Result<RestoredSpace> {
        ensure!(
            removed.chat().is_some(),
            "the export carried a workspace as a chat"
        );
        let repo = match selector {
            Some(selector) => Some(
                (self.repository(selector).await)
                    .map_err(|error| Refusal::wrap(Reason::NotImported, error))?,
            ),
            None => None,
        };
        let removed = self.imported_record(repo.as_ref(), removed).await?;
        let id = removed.space_id.clone();
        self.store
            .write(move |tx| record_imported_chat(tx, &removed))
            .await?;
        self.restore_space(&SpaceSelector::id(&id), None)
            .await
            .with_context(|| {
                format!("the imported chat is recorded as removed; shoal restore {id} retries")
            })
    }

    /// Record the packaged `removed` as a removed workspace of the
    /// repository `selector` names, as [`Self::import_workspace`] does
    /// before opening it; an error leaves no record.
    async fn record_package(
        &self,
        selector: &crate::repository::Selector,
        bundle: Option<&Path>,
        prerequisite: Option<&str>,
        removed: RemovedSpace,
        copy: bool,
        session_files: &[CarriedSessionFile],
    ) -> Result<RemovedSpace> {
        let repo = self.repository(selector).await?;
        ensure!(
            removed.kept_workspace()?.checkpoint.workspace_id == removed.space_id,
            "the export's checkpoint belongs to another workspace"
        );
        if let Some(commit) = prerequisite {
            self.fetch_missing(&repo, &commit.into()).await?;
        }
        let removed = self.imported_record(Some(&repo), removed).await?;
        // A repository move holds this gate; the checkout may have moved.
        let (repo, _git) = self.lock_repository(&repo.id).await?;
        self.record_import(&repo, bundle, removed, copy, session_files)
            .await
    }

    /// Fetch the default branch when the repository lacks `commit`, which
    /// the export builds on.
    async fn fetch_missing(&self, repo: &Repository, commit: &CommitId) -> Result<()> {
        if git::has_commit(&repo.path, commit.as_str()).await? {
            return Ok(());
        }
        self.fetch_default_branch(&repo.id)
            .await
            .context("fetch the default branch the export builds on")
    }

    /// The exported record as this repository's, or a chat's without one,
    /// expiring after its retention as a removal here would, a workspace's
    /// checkpoint with it; a chat without a repository runs in `root_dir`,
    /// which this prepares as a chat's start does.
    async fn imported_record(
        &self,
        repo: Option<&Repository>,
        removed: RemovedSpace,
    ) -> Result<RemovedSpace> {
        let settings = self.space_settings_in(repo.map(|repo| &repo.id)).await?;
        let retention = settings.cleanup.restore.retention();
        let expires_at = UnixTime::now() + retention;
        // The space it was below where it was exported stays its parent only
        // where that space is here too.
        let parent = match removed.parent {
            Some(parent) if self.find_space(&parent.id).await?.is_some() => Some(parent),
            _ => None,
        };
        let mut removed = RemovedSpace {
            repository_id: repo.map(|repo| repo.id.clone()),
            expires_at,
            parent,
            ..removed
        };
        match (&mut removed.kind, repo) {
            (RemovedKind::Workspace(kept), Some(repo)) => {
                kept.checkpoint.repository_id = repo.id.clone();
                kept.checkpoint.expires_at = expires_at;
            }
            (RemovedKind::Workspace(_), None) => bail!("a workspace belongs to a repository"),
            (RemovedKind::Chat { chat }, Some(_)) => chat.root_dir = None,
            (RemovedKind::Chat { chat }, None) => {
                chat.root_dir = Some(self.prepare_chat_root().await?);
            }
        }
        Ok(removed)
    }

    /// The exported `removed` as a new workspace of `repo` that copies it:
    /// with its own ID, on a branch of its own named after the original's,
    /// created at its HEAD, and without the PRs and CI runs, which stay with
    /// the original's branch. The caller holds the repository's Git gate,
    /// so a concurrent import takes another branch.
    async fn copied_record(
        &self,
        repo: &Repository,
        removed: RemovedSpace,
    ) -> Result<RemovedSpace> {
        let id = SpaceId::generate();
        let mut removed = removed;
        let checkpoint = &mut removed.kept_workspace_mut()?.checkpoint;
        checkpoint.workspace_id = id.clone();
        let name = match &checkpoint.branch {
            Some(branch) => {
                let branch = self.copy_branch(repo, branch.as_str()).await?;
                git::branch::create(&repo.path, &branch, checkpoint.head.as_str()).await?;
                checkpoint.branch = Some(branch.as_str().into());
                SpaceName::derive(branch.as_str(), "workspace")
            }
            None => removed.name.clone(),
        };
        Ok(RemovedSpace {
            space_id: id,
            name,
            links: removed.links.copied(),
            ..removed
        })
    }

    /// Point the checkpoint's ref at its commit, keep the agents'
    /// `session_files`, then record the checkpoint and the removal in one
    /// transaction, as a removal would; a `copy` as the new workspace that
    /// copies it. The caller holds the repository's Git gate.
    async fn record_import(
        &self,
        repo: &Repository,
        bundle: Option<&Path>,
        removed: RemovedSpace,
        copy: bool,
        session_files: &[CarriedSessionFile],
    ) -> Result<RemovedSpace> {
        let _refs = self.gates.lock_checkpoints().await;
        let commit = &removed.kept_workspace()?.checkpoint.commit;
        let reference = git::checkpoint::reference(commit);
        match bundle {
            Some(bundle) => git::bundle::fetch(&repo.path, bundle, &reference).await?,
            None => git::checkpoint::save(&repo.path, &reference, commit)
                .await
                .with_context(|| format!("the default branch has no commit {commit}"))?,
        }
        let removed = match copy {
            true => self.copied_record(repo, removed).await?,
            false => removed,
        };
        let removed = self
            .keep_imported_sessions(repo, removed, session_files)
            .await?;
        let record = removed.clone();
        if let Err(error) = self
            .store
            .write(move |tx| record_imported(tx, record))
            .await
        {
            let _ = self
                .discard_imported_sessions(repo, &removed.space_id)
                .await;
            return Err(error);
        }
        Ok(removed)
    }
}

/// Record an imported chat's removal record, unless its repository is being
/// removed.
fn record_imported_chat(tx: &rusqlite::Transaction<'_>, removed: &RemovedSpace) -> Result<()> {
    if let Some(repository) = &removed.repository_id {
        anyhow::ensure!(
            !crate::daemon::store::repository_removing(tx, repository)?,
            "the repository is being removed"
        );
    }
    super::records::record(tx, removed)
}

/// Record an imported workspace's checkpoint under its next number and its
/// removal record naming that number, unless the repository is being removed.
fn record_imported(tx: &rusqlite::Transaction<'_>, mut removed: RemovedSpace) -> Result<()> {
    anyhow::ensure!(
        !crate::daemon::store::repository_removing(tx, removed.repository()?)?,
        "the repository is being removed"
    );
    let checkpoint = &mut removed.kept_workspace_mut()?.checkpoint;
    checkpoint.number = super::super::checkpoints::insert_numbered(tx, checkpoint)?;
    super::records::record(tx, &removed)?;
    Ok(())
}
