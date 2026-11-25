//! The configuration layers of a repository's spaces, and where its local
//! config file lives.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use shoal_core::{
    config::{
        self, Config, Effective,
        repo::{ConfigLayers, Rejected, RepoConfig},
    },
    fsutil::off_runtime,
    repository::Selector,
};
use shoal_daemon_base::log;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{
    model::{LocalConfigEdit, Repository, Space, Workspace},
    protocol::ConfigTarget,
    space_names::SpaceSelector,
};

use crate::{Repositories, daemon::Daemon};

mod copies;
mod rejected;
pub(crate) use rejected::RejectedConfigs;

impl Repositories {
    /// The repository's local config file.
    pub async fn local_config_path(&self, selector: impl Into<Selector>) -> Result<PathBuf> {
        let repo = self.repository(selector).await?;
        self.local_config_outside_checkouts(&repo.workspaces_dir)
            .await
    }

    /// Apply `changes` to the repository's local config, saving only a valid
    /// result. It holds the gate moves and removal take, so the file stays
    /// where the edit found it.
    pub async fn edit_local_config(
        &self,
        daemon: &dyn Daemon,
        selector: impl Into<Selector>,
        changes: Vec<config::edit::Change>,
    ) -> Result<LocalConfigEdit> {
        let _registry = self.gates.lock_registry().await;
        let repo = self.repository(selector).await?;
        self.ensure_available(&repo.id).await?;
        let path = (self.local_config_outside_checkouts(&repo.workspaces_dir)).await?;
        let (config, backup) = tokio::task::spawn_blocking(move || {
            shoal_core::config_file::edit(path, &changes, |edited| {
                config::repo::parse(edited)
                    .map(drop)
                    .context("invalid edited local config")
            })
        })
        .await??;
        daemon.local_config_changed();
        Ok(LocalConfigEdit { config, backup })
    }

    /// [`Self::workspace_settings`] with the mode and rule files beside the
    /// global config, which a review's instructions and a launch's mode read,
    /// over the daemon's current global config.
    pub async fn workspace_settings_with_templates(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<Effective> {
        let layers = self.workspace_layers(daemon, workspace).await?;
        self.config()
            .resolve_with_template_files(&layers, &self.paths)
    }

    /// The layers of `workspace`, without a worktree file the current format
    /// rejects, which they name instead: a branch from before a format
    /// change keeps its old file, and the other layers still settle what the
    /// workspace does. The user hears of each new rejection once.
    pub async fn workspace_layers(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<ConfigLayers> {
        let (file, rejected) = match self.worktree_file(&workspace.path).await {
            Err(error) if Rejected::is(&error) => {
                self.report_rejected_config(daemon, workspace, &error).await;
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
                log!(
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
    pub async fn config_layers_for(
        &self,
        daemon: &dyn Daemon,
        target: ConfigTarget,
    ) -> Result<ConfigLayers> {
        match target {
            ConfigTarget::Global => Ok(ConfigLayers::default()),
            ConfigTarget::Space(selector) => {
                let space = daemon.space(&selector).await?;
                self.space_layers(daemon, &space, None).await
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
        daemon: &dyn Daemon,
        space: &Space,
        repository: Option<&RepositoryId>,
    ) -> Result<ConfigLayers> {
        match space.to_workspace() {
            Some(workspace) => self.workspace_layers(daemon, &workspace).await,
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

    /// The settings of `target` with the mode and rule files beside the
    /// global config, which a launch's mode may come from.
    pub async fn target_settings_with_templates(
        &self,
        daemon: &dyn Daemon,
        target: ConfigTarget,
    ) -> Result<Effective> {
        let layers = self.config_layers_for(daemon, target).await?;
        (self.config()).resolve_with_template_files(&layers, &self.paths)
    }

    /// The settings of `space` after every layer of [`Self::space_layers`],
    /// over the daemon's current global config.
    pub async fn space_settings(&self, daemon: &dyn Daemon, space: &Space) -> Result<Effective> {
        self.space_settings_from(daemon, &self.config(), space, None)
            .await
    }

    /// [`Self::space_settings`] with the mode and rule files beside the
    /// global config, which a launch's mode may come from; `repository` is
    /// the one an action is in.
    pub async fn space_settings_with_templates(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        repository: Option<&RepositoryId>,
    ) -> Result<Effective> {
        let layers = self.space_layers(daemon, space, repository).await?;
        self.config()
            .resolve_with_template_files(&layers, &self.paths)
    }

    /// The settings of `space` after every layer of [`Self::space_layers`],
    /// over a global snapshot the caller also reads, so one operation never
    /// combines two versions; `repository` is the one an action is in.
    pub async fn space_settings_from(
        &self,
        daemon: &dyn Daemon,
        config: &Config,
        space: &Space,
        repository: Option<&RepositoryId>,
    ) -> Result<Effective> {
        config.resolve(&self.space_layers(daemon, space, repository).await?)
    }

    /// The settings of a space outside a worktree in the repository `id`, or
    /// the global config's alone for a space without one.
    pub async fn space_settings_in(&self, id: Option<&RepositoryId>) -> Result<Effective> {
        self.config().resolve(&self.repository_layers_in(id).await?)
    }

    /// The layers of the repository `selector` names, with the file in its
    /// registered checkout.
    async fn checkout_layers(&self, selector: Selector) -> Result<ConfigLayers> {
        let repo = self.repository(&selector).await?;
        let file = self.worktree_file(&repo.path).await?;
        self.config_layers(&repo.id, file).await
    }

    /// The config file the checkout of `repository` tracks, as TOML, which
    /// another machine reads it by before it has the repository.
    pub async fn tracked_config(&self, repository: &Repository) -> Result<String> {
        let (file, _) = self.worktree_file(&repository.path).await?;
        Ok(toml::to_string(&file)?)
    }

    /// The config file the repository of the space `selector` tracks, as
    /// [`Self::tracked_config`] reads it.
    pub async fn space_tracked_config(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<String> {
        let space = daemon.space(selector).await?;
        let id =
            (space.repository_id).with_context(|| format!("{} has no repository", space.name))?;
        self.tracked_config(&self.repository(&id).await?).await
    }

    /// The settings of a repository this machine has none of, whose
    /// `tracked` config another machine sent, over the global config.
    pub fn tracked_settings(&self, tracked: &str) -> Result<Effective> {
        let worktree_file = toml::from_str::<RepoConfig>(tracked)
            .context("the repository's config does not fit this machine's Shoal")?;
        let layers = ConfigLayers {
            worktree_file,
            ..Default::default()
        };
        (self.config()).resolve_with_template_files(&layers, &self.paths)
    }

    /// The config file and template files of the checkout or worktree at
    /// `path`, read under `FileAccess::checked`, since reading them waits
    /// on any privacy prompt for its volume.
    async fn worktree_file(&self, path: &Path) -> Result<(RepoConfig, config::modes::Files)> {
        self.file_access
            .checked(&self.notifications, path, load_worktree_file(path))
            .await
    }

    /// The local config file in the repository directory `directory`, unless
    /// a checkout or worktree lies where that file's directory belongs.
    async fn local_config_outside_checkouts(&self, directory: &Path) -> Result<PathBuf> {
        if let Some(occupant) = self.local_config_occupant(directory).await? {
            bail!(
                "{} is a checkout where the local config belongs; register it in another directory",
                occupant.display()
            );
        }
        Ok(config::repo::local_config_path(directory))
    }

    /// The file beside the repository's local config that keeps the claims
    /// its reviews raised and that were resolved as wrong; none while a
    /// checkout lies where the local config belongs.
    pub async fn false_positives_path(
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
    pub async fn writable_false_positives_path(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<PathBuf> {
        let repository = self.repository(repository_id).await?;
        self.ensure_available(&repository.id).await?;
        let directory = &repository.workspaces_dir;
        self.local_config_outside_checkouts(directory).await?;
        Ok(config::repo::false_positives_path(directory))
    }

    /// A checkout or worktree, recorded or not, that lies where the local
    /// config of the repository directory `directory` belongs: its tracked
    /// files are never local settings.
    async fn local_config_occupant(&self, directory: &Path) -> Result<Option<PathBuf>> {
        let local = directory.join(config::repo::LOCAL_DIR);
        let git = local.join(".git");
        if off_runtime::run(move || Ok(std::fs::symlink_metadata(git).is_ok())).await? {
            return Ok(Some(local));
        }
        self.store
            .run(move |db| crate::contents::checkout_within(db, &local))
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
    pub async fn workspace_settings(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<Effective> {
        self.workspace_settings_from(daemon, &self.config(), workspace)
            .await
    }

    /// [`Self::workspace_settings`] over a global snapshot the caller also
    /// reads, so one operation never combines two versions.
    pub async fn workspace_settings_from(
        &self,
        daemon: &dyn Daemon,
        config: &Config,
        workspace: &Workspace,
    ) -> Result<Effective> {
        config.resolve(&self.workspace_layers(daemon, workspace).await?)
    }

    /// The repository's settings after every layer, with the file in its
    /// registered checkout.
    pub async fn repository_settings(&self, id: &RepositoryId) -> Result<Effective> {
        self.repository_settings_from(&self.config(), id).await
    }

    /// [`Self::repository_settings`] with the mode and rule files beside the
    /// global config, which a launch's mode may come from.
    pub async fn repository_settings_with_templates(&self, id: &RepositoryId) -> Result<Effective> {
        let layers = self.checkout_layers(id.into()).await?;
        self.config()
            .resolve_with_template_files(&layers, &self.paths)
    }

    /// [`Self::repository_settings`] over a global snapshot the caller also
    /// reads, so one operation never combines two versions.
    pub async fn repository_settings_from(
        &self,
        config: &Config,
        id: &RepositoryId,
    ) -> Result<Effective> {
        config.resolve(&self.checkout_layers(id.into()).await?)
    }
}

/// The config file and template files of the checkout or worktree at `path`.
async fn load_worktree_file(path: &Path) -> Result<(RepoConfig, config::modes::Files)> {
    let path = path.to_owned();
    off_runtime::run(move || config::repo::load(&path)).await
}

#[cfg(test)]
mod tests;
