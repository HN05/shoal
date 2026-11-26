//! Copies of the files a proposal attaches, kept until it is decided, since
//! the agent's own may be gone by then.
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use shoal_core::shoal_dirs::{Contents, ShoalDir};
use shoal_daemon_base::{file_copies::keep_copies, log};
use shoal_forge::items::attach;
use shoal_forge_types::items::attach::Attachment;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    approvals::{ApprovalRequest, DecisionStatus},
    model::Repository,
};

use crate::{Links, daemon::Daemon};

impl Links {
    /// Copy the files a proposal for the repository `repository` attaches
    /// into its directory of proposal files and point `body` at the copies.
    /// The directory is named by the proposal's content, files included, so
    /// proposing it again reuses the copies and finds the same request, and
    /// copies never change. The caller holds the `proposal_files` gate until
    /// the proposal is stored, since pruning takes it too.
    pub async fn keep_attachments(
        &self,
        daemon: &dyn Daemon,
        owner: &SpaceId,
        repository: &RepositoryId,
        body: &mut String,
        attachments: &mut Vec<Attachment>,
    ) -> Result<()> {
        if attachments.is_empty() {
            return Ok(());
        }
        let repository = daemon.repository(repository).await?;
        let dir = ShoalDir::of(&repository, Contents::ProposalFiles);
        let (owner, text, originals) = (owner.to_owned(), body.clone(), attachments.clone());
        let copies = tokio::task::spawn_blocking(move || {
            dir.create()?;
            keep_copies(dir.root(), &owner, &text, &originals).map(|copies| copies.attachments)
        })
        .await??;
        *body = attach::retarget(body, attachments, &copies);
        *attachments = copies;
        Ok(())
    }

    /// Delete the repository's copies with its registration.
    pub async fn delete_proposal_files(&self, repository: &Repository) -> Result<()> {
        let _files = self.gates.lock_proposal_files().await;
        let dir = ShoalDir::of(repository, Contents::ProposalFiles);
        tokio::task::spawn_blocking(move || dir.remove()).await?
    }

    /// [`Self::prune_proposal_files`] for the cleanup sweep, over every
    /// repository's copies, which also catches proposals removed with their
    /// workspace or repository.
    pub async fn sweep_proposal_files(&self, daemon: &dyn Daemon) -> Result<()> {
        let _files = self.gates.lock_proposal_files().await;
        let dirs = (daemon.repositories().await?.iter())
            .map(|repository| ShoalDir::of(repository, Contents::ProposalFiles))
            .collect();
        self.remove_unattached_files(dirs).await
    }

    /// Delete the copies no pending proposal attaches any more, among those
    /// of the repository `request` is for. Failure only logs.
    pub async fn prune_proposal_files(&self, daemon: &dyn Daemon, request: &ApprovalRequest) {
        let _files = self.gates.lock_proposal_files().await;
        let pruned = async {
            let dirs = match self.proposal_repository(daemon, request).await {
                Some(id) => vec![ShoalDir::of(
                    &daemon.repository(&id).await?,
                    Contents::ProposalFiles,
                )],
                None => Vec::new(),
            };
            self.remove_unattached_files(dirs).await
        };
        if let Err(error) = pruned.await {
            log!("proposal files not pruned: {error:#}");
        }
    }

    /// The repository `request` is for: an issue proposal's own, or the
    /// proposing workspace's while it exists.
    async fn proposal_repository(
        &self,
        daemon: &dyn Daemon,
        request: &ApprovalRequest,
    ) -> Option<RepositoryId> {
        match &request.repository_id {
            Some(repository) => Some(repository.clone()),
            None => (daemon.workspace_by_id(&request.space_id).await.ok())
                .and_then(|workspace| workspace.repository_id),
        }
    }

    /// Delete the copies in `dirs` that no pending proposal attaches.
    async fn remove_unattached_files(&self, dirs: Vec<ShoalDir>) -> Result<()> {
        let attached: Vec<PathBuf> = (self.approval_requests(None).await?.iter())
            .filter(|request| request.status == DecisionStatus::Pending)
            .flat_map(ApprovalRequest::attachments)
            .map(|attachment| attachment.path.clone())
            .collect();
        tokio::task::spawn_blocking(move || {
            for dir in dirs.iter().filter(|dir| dir.is_own()) {
                remove_unattached(dir.root(), &attached)?;
            }
            Ok(())
        })
        .await?
    }
}

/// Remove the directories of copies below `root` that hold none of the
/// files `attached`.
fn remove_unattached(root: &Path, attached: &[PathBuf]) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(());
    };
    let kept: BTreeSet<OsString> = attached
        .iter()
        .filter_map(|path| path.strip_prefix(root).ok())
        .filter_map(|path| path.components().next())
        .map(|component| component.as_os_str().to_owned())
        .collect();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !kept.contains(&entry.file_name()) {
            std::fs::remove_dir_all(entry.path())
                .with_context(|| format!("remove {}", entry.path().display()))?;
        }
    }
    Ok(())
}
