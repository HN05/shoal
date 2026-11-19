//! The configuration layers of a repository's workspaces, and where its
//! local config file lives.
use super::manager::Manager;
use crate::config::hooks::HookCommand;
use crate::{
    config::{
        self, Config, Effective,
        repo::{ConfigLayers, Rejected, RepoConfig},
    },
    fsutil::off_runtime,
    hooks::{HookDirectory, HookKind},
    model::{LocalConfigEdit, Part, Space, Workspace},
    protocol::ConfigTarget,
};
use anyhow::{Context, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

mod rejected;
pub(crate) use rejected::RejectedConfigs;

impl Manager {
    /// The repository's local config file.
    pub async fn local_config_path(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<PathBuf> {
        let repo = self.repository(selector).await?;
        self.local_config_outside_checkouts(&repo.workspaces_dir)
            .await
    }

    /// Apply `changes` to the repository's local config, saving only a valid
    /// result. It holds the gate moves and removal take, so the file stays
    /// where the edit found it.
    pub async fn edit_local_config(
        &self,
        selector: impl Into<crate::repository::Selector>,
        changes: Vec<config::edit::Change>,
    ) -> Result<LocalConfigEdit> {
        let _registry = self.gates.lock_registry().await;
        let repo = self.repository(selector).await?;
        self.ensure_repository_available(&repo.id).await?;
        let path = (self.local_config_outside_checkouts(&repo.workspaces_dir)).await?;
        let (config, backup) = tokio::task::spawn_blocking(move || {
            crate::config_file::edit(path, &changes, |edited| {
                config::repo::parse(edited)
                    .map(drop)
                    .context("invalid edited local config")
            })
        })
        .await??;
        // The runner follows `runner.enabled` as soon as it changes.
        self.runners.settings_changed();
        Ok(LocalConfigEdit { config, backup })
    }

    /// [`Self::workspace_hook`] of the workspace `selector` names.
    pub(crate) async fn selected_workspace_hook(
        &self,
        selector: &SpaceSelector,
        kind: HookKind,
    ) -> Result<Option<std::path::PathBuf>> {
        let workspace = self.workspace(selector).await?;
        self.workspace_hook(&workspace, kind).await
    }

    /// A hook's executable from the resolved settings, in the directory it
    /// runs from.
    pub(crate) async fn workspace_hook(
        &self,
        workspace: &Workspace,
        kind: HookKind,
    ) -> Result<Option<std::path::PathBuf>> {
        let layers = self.workspace_layers(workspace).await?;
        ensure_lease_hook_readable(kind, layers.rejected_worktree_file.as_deref())?;
        let settings = self.config().resolve(&layers)?;
        let Some(command) = kind.command(&settings) else {
            return Ok(None);
        };
        let repository = self.repository(workspace.repository()).await?;
        // The caller renders the hook's [env], whose caches only the daemon
        // creates, with the temporary directory, which is gone after removal.
        crate::caches::directories(&repository, &settings.caches)?;
        if !matches!(kind, HookKind::PostRemove) {
            crate::tmp_dirs::directory(&repository, &workspace.id)?;
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

    /// [`Self::workspace_settings`] with the mode and rule files beside the
    /// global config, which a review's instructions and a launch's mode read,
    /// over the daemon's current global config.
    pub(crate) async fn workspace_settings_with_templates(
        &self,
        workspace: &Workspace,
    ) -> Result<Effective> {
        let layers = self.workspace_layers(workspace).await?;
        self.config()
            .resolve_with_template_files(&layers, &self.paths)
    }

    /// The layers of `workspace`, without a worktree file the current format
    /// rejects, which they name instead: a branch from before a format
    /// change keeps its old file, and the other layers still settle what the
    /// workspace does. The user hears of each new rejection once.
    async fn workspace_layers(&self, workspace: &Workspace) -> Result<ConfigLayers> {
        let (file, rejected) = match self.worktree_file(&workspace.path).await {
            Err(error) if Rejected::is(&error) => {
                self.report_rejected_config(workspace, &error).await;
                (Default::default(), Some(format!("{error:#}")))
            }
            read => {
                let file = read?;
                self.rejected_configs.forget(&workspace.id);
                (file, None)
            }
        };
        let layers = self.config_layers(workspace.repository(), file).await?;
        let signing_secrets =
            (self.withheld_signing_secrets(workspace, &layers.local_config)).await;
        Ok(ConfigLayers {
            rejected_worktree_file: rejected,
            signing_secrets,
            ..layers
        })
    }

    /// The secrets that `workspace`'s agents may not receive beyond those
    /// its own layers' triggers sign with: those of the checkout file's
    /// webhook triggers, which fire whatever the workspace's own file says.
    /// While that file cannot be read, its triggers could sign with any
    /// secret of the `local` config, the only ones a repository's trigger
    /// takes, so none of those reaches an agent until it reads.
    async fn withheld_signing_secrets(
        &self,
        workspace: &Workspace,
        local: &RepoConfig,
    ) -> BTreeSet<String> {
        match self.checkout_signing_secrets(workspace.repository()).await {
            Ok(signing) => signing,
            Err(error) => {
                crate::daemon::log!(
                    "withholding the local config's secrets from {}'s agents: the checkout's triggers cannot be read: {error:#}",
                    workspace.id
                );
                local.secrets.keys().cloned().collect()
            }
        }
    }

    /// The secrets the webhook triggers of `repository`'s checkout file sign
    /// with, which fire whatever a workspace's own file says.
    async fn checkout_signing_secrets(
        &self,
        repository: &RepositoryId,
    ) -> Result<BTreeSet<String>> {
        let checkout = self.repository(repository).await?;
        let (file, _) = self.worktree_file(&checkout.path).await?;
        Ok((file.triggers.values())
            .filter_map(|trigger| trigger.signing_secret().map(str::to_owned))
            .collect())
    }

    /// The repository layer for `target`; a repository's file is the one in
    /// its registered checkout, which is a chat's too, and the global
    /// config has none.
    pub async fn config_layers_for(&self, target: ConfigTarget) -> Result<ConfigLayers> {
        match target {
            ConfigTarget::Global => Ok(ConfigLayers::default()),
            ConfigTarget::Space(selector) => {
                self.space_layers(&self.space(&selector).await?, None).await
            }
            ConfigTarget::Repository(selector) => self.checkout_layers(selector).await,
        }
    }

    /// The layers of `space`: a workspace's with the file in its worktree,
    /// any other space's with the file in the registered checkout of its
    /// repository, or for a space without one of `repository`, the one an
    /// action is in, else none.
    async fn space_layers(
        &self,
        space: &Space,
        repository: Option<&RepositoryId>,
    ) -> Result<ConfigLayers> {
        match space.to_workspace() {
            Some(workspace) => self.workspace_layers(&workspace).await,
            None => {
                self.repository_layers_in(space.repository_id.as_ref().or(repository))
                    .await
            }
        }
    }

    /// The layers of a space outside a worktree in the repository `id`, with
    /// the file in its registered checkout; none for a space without one.
    async fn repository_layers_in(&self, id: Option<&RepositoryId>) -> Result<ConfigLayers> {
        match id {
            Some(id) => self.checkout_layers(id.into()).await,
            None => Ok(ConfigLayers::default()),
        }
    }

    /// The settings of `space` after every layer of [`Self::space_layers`],
    /// over the daemon's current global config.
    pub(crate) async fn space_settings(&self, space: &Space) -> Result<Effective> {
        self.space_settings_from(&self.config(), space, None).await
    }

    /// [`Self::space_settings`] with the mode and rule files beside the
    /// global config, which a launch's mode may come from; `repository` is
    /// the one an action is in.
    pub(crate) async fn space_settings_with_templates(
        &self,
        space: &Space,
        repository: Option<&RepositoryId>,
    ) -> Result<Effective> {
        let layers = self.space_layers(space, repository).await?;
        self.config()
            .resolve_with_template_files(&layers, &self.paths)
    }

    /// The settings of `space` after every layer of [`Self::space_layers`],
    /// over a global snapshot the caller also reads, so one operation never
    /// combines two versions; `repository` is the one an action is in.
    pub(crate) async fn space_settings_from(
        &self,
        config: &Config,
        space: &Space,
        repository: Option<&RepositoryId>,
    ) -> Result<Effective> {
        config.resolve(&self.space_layers(space, repository).await?)
    }

    /// The settings of a space outside a worktree in the repository `id`, or
    /// the global config's alone for a space without one.
    pub(crate) async fn space_settings_in(&self, id: Option<&RepositoryId>) -> Result<Effective> {
        self.config().resolve(&self.repository_layers_in(id).await?)
    }

    /// The layers of the repository `selector` names, with the file in its
    /// registered checkout.
    async fn checkout_layers(&self, selector: crate::repository::Selector) -> Result<ConfigLayers> {
        let repo = self.repository(&selector).await?;
        let file = self.worktree_file(&repo.path).await?;
        self.config_layers(&repo.id, file).await
    }

    /// The config file and template files of the checkout or worktree at
    /// `path`, read under `FileAccess::checked`, since reading them waits
    /// on any privacy prompt for its volume.
    async fn worktree_file(
        &self,
        path: &std::path::Path,
    ) -> Result<(RepoConfig, config::modes::Files)> {
        self.file_access
            .checked(&self.notifications, path, load_worktree_file(path))
            .await
    }

    /// The local config file in the repository directory `directory`, unless
    /// a checkout or worktree lies where that file's directory belongs.
    async fn local_config_outside_checkouts(&self, directory: &std::path::Path) -> Result<PathBuf> {
        if let Some(occupant) = self.local_config_occupant(directory).await? {
            anyhow::bail!(
                "{} is a checkout where the local config belongs; register it in another directory",
                occupant.display()
            );
        }
        Ok(config::repo::local_config_path(directory))
    }

    /// The file beside the repository's local config that keeps the claims
    /// its reviews raised and that were resolved as wrong; none while a
    /// checkout lies where the local config belongs.
    pub(crate) async fn false_positives_path(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Option<PathBuf>> {
        let repository = self.repository(repository_id).await?;
        let directory = &repository.workspaces_dir;
        Ok((self.local_config_occupant(directory).await?)
            .is_none()
            .then(|| config::repo::false_positives_path(directory)))
    }

    /// [`Self::false_positives_path`] for an entry to add, refusing a
    /// checkout where the local config belongs. The caller holds the
    /// registry gate, as local config edits do.
    pub(crate) async fn writable_false_positives_path(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<PathBuf> {
        let repository = self.repository(repository_id).await?;
        self.ensure_repository_available(&repository.id).await?;
        let directory = &repository.workspaces_dir;
        self.local_config_outside_checkouts(directory).await?;
        Ok(config::repo::false_positives_path(directory))
    }

    /// A checkout or worktree, recorded or not, that lies where the local
    /// config of the repository directory `directory` belongs: its tracked
    /// files are never local settings.
    async fn local_config_occupant(&self, directory: &std::path::Path) -> Result<Option<PathBuf>> {
        let local = directory.join(config::repo::LOCAL_DIR);
        let git = local.join(".git");
        if off_runtime::run(move || Ok(std::fs::symlink_metadata(git).is_ok())).await? {
            return Ok(Some(local));
        }
        self.store
            .run(move |db| crate::daemon::store::checkout_within(db, &local))
            .await
    }

    async fn config_layers(
        &self,
        repository_id: &RepositoryId,
        (worktree_file, worktree_template_files): (RepoConfig, config::modes::Files),
    ) -> Result<ConfigLayers> {
        let repository = self.repository(repository_id).await?;
        // Registration and workspace creation keep checkouts out of the
        // local config's directory, so one there predates the local config,
        // which therefore has nothing to hold, and edits refuse.
        let directory = repository.workspaces_dir;
        let (local_config, local_template_files) =
            match self.local_config_occupant(&directory).await? {
                None => off_runtime::run(move || config::repo::load_local(&directory)).await?,
                Some(_) => Default::default(),
            };
        Ok(ConfigLayers {
            worktree_file,
            local_config,
            worktree_template_files,
            local_template_files,
            rejected_worktree_file: None,
            signing_secrets: BTreeSet::new(),
        })
    }

    /// The workspace's settings after every layer: the local config, the
    /// worktree file and the global config.
    pub(crate) async fn workspace_settings(&self, workspace: &Workspace) -> Result<Effective> {
        self.workspace_settings_from(&self.config(), workspace)
            .await
    }

    /// [`Self::workspace_settings`] over a global snapshot the caller also
    /// reads, so one operation never combines two versions.
    pub(crate) async fn workspace_settings_from(
        &self,
        config: &Config,
        workspace: &Workspace,
    ) -> Result<Effective> {
        config.resolve(&self.workspace_layers(workspace).await?)
    }

    /// The repository's settings after every layer, with the file in its
    /// registered checkout.
    pub(crate) async fn repository_settings(&self, id: &RepositoryId) -> Result<Effective> {
        self.repository_settings_from(&self.config(), id).await
    }

    /// [`Self::repository_settings`] with the mode and rule files beside the
    /// global config, which a launch's mode may come from.
    pub(crate) async fn repository_settings_with_templates(
        &self,
        id: &RepositoryId,
    ) -> Result<Effective> {
        let layers = self
            .config_layers_for(ConfigTarget::Repository(id.into()))
            .await?;
        self.config()
            .resolve_with_template_files(&layers, &self.paths)
    }

    /// [`Self::repository_settings`] over a global snapshot the caller also
    /// reads, so one operation never combines two versions.
    pub(crate) async fn repository_settings_from(
        &self,
        config: &Config,
        id: &RepositoryId,
    ) -> Result<Effective> {
        let layers = self
            .config_layers_for(ConfigTarget::Repository(id.into()))
            .await?;
        config.resolve(&layers)
    }

    /// What `[env]` of `settings` and the temporary directory of `space`,
    /// created when missing, add to the environment of its hooks, which act
    /// in the repository `repository`: its own, or the registered one a chat
    /// without one acts in.
    pub(crate) async fn hook_environment<P: Part>(
        &self,
        space: &Space<P>,
        repository: &RepositoryId,
        settings: &Effective,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        (self.environment_from_settings(space, repository, settings, true)).await
    }

    /// What `[env]` adds to the environment of the post-remove hook, which
    /// runs once the temporary directory is gone and so keeps the inherited
    /// `TMPDIR`.
    pub(crate) async fn removed_workspace_environment(
        &self,
        workspace: &Workspace,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        let settings = self.workspace_settings(workspace).await?;
        let repository = workspace.repository();
        (self.environment_from_settings(workspace, repository, &settings, false)).await
    }

    async fn environment_from_settings<P: Part>(
        &self,
        space: &Space<P>,
        repository: &RepositoryId,
        settings: &Effective,
        with_tmp_dir: bool,
    ) -> Result<BTreeMap<String, std::ffi::OsString>> {
        let repository = self.repository(repository).await?;
        let id = space.id.clone();
        let ports = (self.store)
            .read(move |tx| crate::daemon::store::ports(tx, Some(&id)))
            .await?;
        let caches = crate::caches::directories(&repository, &settings.caches)?;
        let tmp_dir = match with_tmp_dir {
            true => {
                let own = space.repository_id.is_some().then_some(&repository);
                (self.tmp_layout(own, space.workdir.root_dir()))
                    .map(|layout| layout.prepare(&space.id))
                    .transpose()?
            }
            false => None,
        };
        Ok(crate::env::render_configured_environment(
            &settings.env,
            space,
            repository.display_name(),
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

/// The config file and template files of the checkout or worktree at `path`.
async fn load_worktree_file(path: &std::path::Path) -> Result<(RepoConfig, config::modes::Files)> {
    let path = path.to_owned();
    off_runtime::run(move || config::repo::load(&path)).await
}

#[cfg(test)]
mod tests;
