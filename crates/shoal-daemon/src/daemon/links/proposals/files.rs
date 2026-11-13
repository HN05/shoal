//! Copies of the files proposed issues and PRs attach, kept until the user
//! or a coordinator decides, since the agent's own may be gone by then. They
//! live in the `.proposals` directory of the repository the proposal is
//! for. Earlier versions kept them in the state directory, where the
//! proposals made then still find them until they are decided.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use anyhow::{Context, Result};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};
use tokio::sync::MutexGuard;

use crate::{
    daemon::{
        approvals::{ApprovalRequest, DecisionStatus},
        file_copies::keep_copies,
        log::log,
        workspace::Manager,
    },
    forge::items::attach::{self, Attachment},
    model::Repository,
    shoal_dirs::{Contents, ShoalDir, remove_if_empty},
};

/// Where earlier versions kept the copies in the state directory.
const OLD_DIRECTORY: &str = "proposals";

impl Manager {
    /// Hold `proposal_files` for a proposal for the repository `repository`
    /// that attaches `attachments`, once a registration older than the
    /// repository's directory has reserved it. The reservation takes the
    /// Git gate, which a repository move holds while it waits for this one.
    pub(super) async fn lock_proposal_files_of(
        &self,
        repository: &RepositoryId,
        attachments: &[Attachment],
    ) -> Result<MutexGuard<'_, ()>> {
        if !attachments.is_empty() {
            self.reserve_repository_directory(repository).await?;
        }
        Ok(self.gates.lock_proposal_files().await)
    }

    /// Copy the files a proposal for the repository `repository` attaches
    /// into its directory of proposal files and point `body` at the copies.
    /// The directory is named by the proposal's content, files included, so
    /// proposing it again reuses the copies and finds the same request, and
    /// copies never change. The caller holds `proposal_files`, from
    /// [`Self::lock_proposal_files_of`], until the proposal is stored, since
    /// pruning takes it too.
    pub(super) async fn keep_attachments(
        &self,
        owner: &SpaceId,
        repository: &RepositoryId,
        body: &mut String,
        attachments: &mut Vec<Attachment>,
    ) -> Result<()> {
        if attachments.is_empty() {
            return Ok(());
        }
        let repository = self.repository(repository).await?;
        let dir = ShoalDir::of(&repository, Contents::ProposalFiles)
            .context("the repository has no directory")?;
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
    pub(crate) async fn delete_proposal_files(&self, repository: &Repository) -> Result<()> {
        let _files = self.gates.lock_proposal_files().await;
        let Some(dir) = ShoalDir::of(repository, Contents::ProposalFiles) else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || dir.remove()).await?
    }

    /// [`Self::prune_proposal_files`] for the cleanup sweep, over every
    /// repository's copies, which also catches proposals removed with their
    /// workspace or repository.
    pub(crate) async fn sweep_proposal_files(&self) -> Result<()> {
        let _files = self.gates.lock_proposal_files().await;
        let dirs = (self.repositories().await?.iter())
            .filter_map(|repository| ShoalDir::of(repository, Contents::ProposalFiles))
            .collect();
        self.remove_unattached_files(dirs).await
    }

    /// Delete the copies no pending proposal attaches any more, among those
    /// of the repository `request` is for. Failure only logs.
    pub(crate) async fn prune_proposal_files(&self, request: &ApprovalRequest) {
        let _files = self.gates.lock_proposal_files().await;
        let pruned = async {
            let dirs = match self.proposal_repository(request).await {
                Some(id) => ShoalDir::of(&self.repository(&id).await?, Contents::ProposalFiles),
                None => None,
            };
            self.remove_unattached_files(dirs.into_iter().collect())
                .await
        };
        if let Err(error) = pruned.await {
            log!("proposal files not pruned: {error:#}");
        }
    }

    /// The repository `request` is for: an issue proposal's own, or the
    /// proposing workspace's while it exists.
    async fn proposal_repository(&self, request: &ApprovalRequest) -> Option<RepositoryId> {
        match &request.repository_id {
            Some(repository) => Some(repository.clone()),
            None => (self.workspace_by_id(&request.space_id).await.ok())
                .map(|workspace| workspace.repository_id),
        }
    }

    /// Delete the copies in `dirs` and in the state directory's old one
    /// that no pending proposal attaches, and the old directory once it
    /// holds none.
    async fn remove_unattached_files(&self, dirs: Vec<ShoalDir>) -> Result<()> {
        let attached: Vec<PathBuf> = (self.approval_requests(None).await?.iter())
            .filter(|request| request.status == DecisionStatus::Pending)
            .flat_map(ApprovalRequest::attachments)
            .map(|attachment| attachment.path.clone())
            .collect();
        let old = self.paths.state.join(OLD_DIRECTORY);
        tokio::task::spawn_blocking(move || {
            for dir in dirs.iter().filter(|dir| dir.is_own()) {
                remove_unattached(dir.root(), &attached)?;
            }
            remove_unattached(&old, &attached)?;
            remove_if_empty(&old)
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
