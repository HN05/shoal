//! Explicitly transfer a linked worktree into Shoal's normal lifecycle.
use super::{
    Manager, derive_workspace_name, existing_base, identity::directory_identity, recorded_base,
};
use crate::daemon::log;
use crate::{
    git::{self, worktrunk},
    model::Workspace,
    state::WorkspaceState,
};
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

impl Manager {
    pub async fn adopt_workspace(
        &self,
        repository: impl Into<crate::repository::Selector>,
        path: &Path,
    ) -> Result<Workspace> {
        self.adopt_workspace_with_mode(repository, path, false)
            .await
    }

    pub async fn copy_workspace(
        &self,
        repository: impl Into<crate::repository::Selector>,
        path: &Path,
    ) -> Result<Workspace> {
        self.adopt_workspace_with_mode(repository, path, true).await
    }

    async fn adopt_workspace_with_mode(
        &self,
        repository: impl Into<crate::repository::Selector>,
        path: &Path,
        copy: bool,
    ) -> Result<Workspace> {
        let (repo, _guard) = self.lock_repository(repository).await?;
        let path = self.workspace_location(&repo, path).await?;
        let trees = git::worktrees(&repo.path, git::run).await?;
        let tree = trees
            .iter()
            .find(|tree| tree.path == path)
            .context("path must be the root of a linked worktree of this repository")?;
        ensure!(
            !tree.locked && !tree.prunable,
            "cannot adopt a locked or prunable worktree"
        );
        let branch = tree
            .branch
            .as_deref()
            .and_then(git::strip_local)
            .context("cannot adopt a detached worktree; check out a branch first")?;
        if copy {
            return self.copy_and_adopt(&repo, &path, branch).await;
        }
        self.adopt_linked_worktree(&repo, path, branch).await
    }

    async fn adopt_linked_worktree(
        &self,
        repo: &crate::model::Repository,
        path: PathBuf,
        branch: &str,
    ) -> Result<Workspace> {
        if let Some(workspace) = self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|w| w.path == path)
        {
            ensure!(
                workspace.repository_id == repo.id && workspace.branch == branch,
                "worktree no longer matches its recorded repository or branch"
            );
            self.reopen_owned_workspace(&workspace, branch).await?;
            return Ok(workspace);
        }
        let git_dir = git::git_dir(&path).await?;
        let identity = directory_identity(&git_dir)?;
        let base = existing_base(repo, branch).await?;
        let (commit, reference) = recorded_base(repo, &base).await?;
        let workspace = Workspace {
            base_commit: Some(commit),
            base_ref: reference,
            git_dir: Some(git_dir.clone()),
            git_dir_id: Some(identity),
            ..Workspace::new_record(
                repo.id.clone(),
                derive_workspace_name(branch),
                path,
                branch.into(),
                WorkspaceState::Ready,
            )
        };
        // A marker naming no recorded workspace outlived its record or a reset
        // state directory; one naming a recorded workspace refuses adoption.
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        self.verify_worktree(&workspace).await?;
        // Record identity, base and readiness together: a crash must never leave
        // an adopted directory with weaker ownership checks or pending setup.
        self.insert_workspace(workspace.clone(), None, None).await?;
        // Mark only once the record exists, so a refused insertion leaves the
        // metadata untouched; the committed identity protects it if marking fails.
        if let Err(error) = self.record_worktree_identity(&workspace).await {
            log!(
                "worktree identity not recorded for {}: {error:#}",
                workspace.name
            );
        }
        Ok(workspace)
    }

    async fn copy_and_adopt(
        &self,
        repo: &crate::model::Repository,
        source: &Path,
        source_branch: &str,
    ) -> Result<Workspace> {
        let branch = self.available_branch(repo, source_branch).await?;
        let name = derive_workspace_name(&branch);
        let destination = self.new_workspace_path(repo, &name, None).await?;
        ensure!(
            !destination.starts_with(source) && !source.starts_with(&destination),
            "copied workspace path overlaps the source worktree"
        );
        let workspace = Workspace::new_record(
            repo.id.clone(),
            name,
            destination,
            branch,
            WorkspaceState::Preparing,
        );
        // Reserve the branch and destination before Worktrunk can mutate Git.
        self.insert_workspace(workspace.clone(), None, None).await?;
        let result = self.materialize_copy(repo, source, &workspace).await;
        match result {
            Ok(()) => self.workspace(&workspace.id).await,
            Err(error) => {
                // Worktrunk can report an error after Git has created the
                // worktree. Capture its identity before retaining the failure.
                if let Err(identity_error) = self.record_copy_identity_if_present(&workspace).await
                {
                    log!(
                        "could not record failed copied workspace {}: {identity_error:#}",
                        workspace.name
                    );
                }
                // Keep the record and any linked worktree for inspection and
                // removal. Startup quarantine applies the same transition if
                // the daemon stops while the copy is in progress.
                let message = format!("{error:#}");
                self.set_state(&workspace.id, WorkspaceState::Failed, Some(message))
                    .await?;
                Err(error).context(format!(
                    "workspace {} failed; inspect or remove it with Shoal",
                    workspace.name
                ))
            }
        }
    }

    /// Create and own the destination before copying user files into it.
    async fn materialize_copy(
        &self,
        repo: &crate::model::Repository,
        source: &Path,
        workspace: &Workspace,
    ) -> Result<()> {
        let commit = git::resolve_commit(source, "HEAD", git::run_isolated).await?;
        worktrunk::create(
            &repo.path,
            &self.paths.worktrunk_config(),
            &workspace.path,
            &workspace.branch,
            Some(&commit),
        )
        .await
        .context("create copied workspace")?;

        // Mark and persist the Git admin directory before the potentially long
        // file copy. A restart can therefore verify and remove the worktree.
        self.record_copy_identity(workspace).await?;
        crate::fsutil::copy_worktree(source, &workspace.path).context("copy worktree files")?;
        self.finish_copy_adoption(repo, workspace).await
    }

    /// Mark and persist the Git admin directory for a copied worktree.
    async fn record_copy_identity(&self, workspace: &Workspace) -> Result<()> {
        let git_dir = git::git_dir(&workspace.path).await?;
        self.release_stale_owner(&git_dir, &workspace.id).await?;
        self.record_worktree_identity(workspace).await?;
        Ok(())
    }

    /// Best-effort identity capture when Worktrunk failed after creating Git data.
    async fn record_copy_identity_if_present(&self, workspace: &Workspace) -> Result<()> {
        if !workspace.path.exists() {
            return Ok(());
        }
        if git::git_dir(&workspace.path).await.is_err() {
            return Ok(());
        }
        self.record_copy_identity(workspace).await
    }

    /// Record the copied worktree's base and readiness after its files arrive.
    async fn finish_copy_adoption(
        &self,
        repo: &crate::model::Repository,
        workspace: &Workspace,
    ) -> Result<()> {
        let base = existing_base(repo, &workspace.branch).await?;
        let (commit, reference) = recorded_base(repo, &base).await?;
        let git_dir = git::git_dir(&workspace.path).await?;
        let identity = directory_identity(&git_dir)?;
        self.record_copy_ready(workspace, git_dir, identity, commit, reference)
            .await
    }

    /// Persist the copied worktree's verified identity, base, and readiness.
    async fn record_copy_ready(
        &self,
        workspace: &Workspace,
        git_dir: PathBuf,
        identity: String,
        commit: String,
        reference: Option<String>,
    ) -> Result<()> {
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET state=?2,error=NULL,git_dir=?3,git_dir_id=?4,base_commit=?5,base_ref=?6,setup_finished=1 WHERE id=?1",
                    rusqlite::params![
                        id,
                        WorkspaceState::Ready,
                        git_dir.to_str(),
                        identity,
                        commit,
                        reference,
                    ],
                )?;
                Ok(())
            })
            .await
    }
}
