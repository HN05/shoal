//! The executables and environment of a space's hooks, which its settings
//! name and configure.
use crate::Spaces;
use crate::daemon::Daemon;
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;

use crate::config::hooks::HookCommand;
use crate::{
    config::Effective,
    hooks::{HookDirectory, HookKind},
    model::{Part, Repository, Space, Workspace},
};

impl Spaces {
    /// [`Self::workspace_hook`] of the workspace `selector` names.
    pub async fn selected_workspace_hook(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        kind: HookKind,
    ) -> Result<Option<PathBuf>> {
        let workspace = self.workspace(selector).await?;
        self.workspace_hook(daemon, &workspace, kind).await
    }

    /// A hook's executable from the resolved settings, in the directory it
    /// runs from.
    pub async fn workspace_hook(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        kind: HookKind,
    ) -> Result<Option<PathBuf>> {
        let layers = daemon.workspace_layers(workspace).await?;
        ensure_lease_hook_readable(kind, layers.rejected_worktree_file.as_deref())?;
        let settings = self.config.current().resolve(&layers)?;
        let Some(command) = kind.command(&settings) else {
            return Ok(None);
        };
        let repository = daemon
            .repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        // The caller renders the hook's [env], whose caches only the daemon
        // creates, with the temporary directory, which is gone after removal.
        crate::caches::directories(&repository, &settings.caches)?;
        if !matches!(kind, HookKind::PostRemove) {
            shoal_core::tmp_dirs::directory(&repository, &workspace.id)?;
        }
        let checkout = match kind.directory() {
            HookDirectory::Worktree => workspace.path.clone(),
            HookDirectory::Checkout => repository.path,
        };
        Ok(Some(
            kind.directory()
                .path(&workspace.path, &checkout)
                .join(command),
        ))
    }

    /// What `[env]` of `settings` and the temporary directory of `space`,
    /// created when missing, add to the environment of its hooks, which act
    /// in the repository `repository`: its own, or the registered one a chat
    /// without one acts in, or none.
    pub async fn hook_environment<P: Part>(
        &self,
        daemon: &dyn Daemon,
        space: &Space<P>,
        repository: Option<&RepositoryId>,
        settings: &Effective,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        (self.environment_from_settings(daemon, space, repository, settings, true)).await
    }

    /// What `[env]` adds to the environment of the post-remove hook, which
    /// runs once the temporary directory is gone and so keeps the inherited
    /// `TMPDIR`.
    pub async fn removed_workspace_environment(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        let settings = daemon.workspace_settings(workspace).await?;
        let repository = Some(workspace.repository());
        (self.environment_from_settings(daemon, workspace, repository, &settings, false)).await
    }

    async fn environment_from_settings<P: Part>(
        &self,
        daemon: &dyn Daemon,
        space: &Space<P>,
        repository: Option<&RepositoryId>,
        settings: &Effective,
        with_tmp_dir: bool,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        let repository = match repository {
            Some(id) => Some(
                daemon
                    .repository(crate::repository::Selector::from(id))
                    .await?,
            ),
            None => None,
        };
        let id = space.id.clone();
        let ports = (self.store)
            .read(move |tx| crate::store::ports(tx, Some(&id)))
            .await?;
        let caches = match &repository {
            Some(repository) => crate::caches::directories(repository, &settings.caches)?,
            None => BTreeMap::new(),
        };
        let tmp_dir = match with_tmp_dir {
            true => {
                let own = (repository.as_ref()).filter(|_| space.repository_id.is_some());
                (self.tmp_layout(own, space.workdir.root_dir()))
                    .map(|layout| layout.prepare(&space.id))
                    .transpose()?
            }
            false => None,
        };
        Ok(crate::env::render_configured_environment(
            &settings.env,
            space,
            repository.as_ref().map(Repository::display_name),
            &ports,
            &caches,
            tmp_dir.as_deref(),
        ))
    }
}

/// Refuse a lease hook while the worktree file, which may name it, is
/// `rejected`: a lease is kept until its release hook ran, so leases neither
/// come nor go without the file. Other hooks go with the ignored file.
fn ensure_lease_hook_readable(kind: HookKind, rejected: Option<&str>) -> Result<()> {
    if let (HookKind::PostResourceAcquire | HookKind::PreResourceRelease, Some(rejected)) =
        (kind, rejected)
    {
        anyhow::bail!(
            "the worktree's config file may name the {} hook, and the current format rejects it; fix the file first: {rejected}",
            kind.key()
        );
    }
    Ok(())
}
