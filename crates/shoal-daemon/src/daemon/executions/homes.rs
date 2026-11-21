//! What an execution takes from the space it runs in, whose records differ
//! by kind: a workspace's verified worktree, ports, caches and configured
//! environment, or a chat's home, its own scope token, its temporary
//! directory and its configured environment without a worktree's.
//! Registration asks nothing else of a space's kind.
use anyhow::{Context as _, Result, ensure};
use rusqlite::Transaction;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::ByKind;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use super::{ExecutionKind, ExecutionKindExt, ProcessDirectories};
use crate::{
    config::Effective,
    daemon::{manager::Manager, scope::tokens, store},
    fsutil::off_runtime,
    model::{Chat, ExecutionSpace, PortReservation, Repository, Space, Workspace},
};

/// The space an execution registers in, read and checked for its plan.
#[allow(
    clippy::large_enum_variant,
    reason = "read once by the registration that made it"
)]
pub(crate) enum Home {
    Workspace {
        workspace: Workspace,
        repository: Repository,
        settings: Effective,
    },
    Chat {
        chat: Chat,
        /// Its repository's registered checkout; none for a chat without
        /// one.
        repository: Option<Repository>,
        /// Its repository's settings, or the global config's alone.
        settings: Effective,
        /// The directory its agent runs in.
        path: PathBuf,
        /// Its temporary directory, created when missing.
        tmp_dir: Option<PathBuf>,
        /// The removals under way, which its registration must not race.
        removing: crate::daemon::spaces::Removing,
    },
}

/// What an execution's plan reads from its space in the transaction that
/// records it.
pub(super) struct Provisions {
    pub(super) ports: Vec<PortReservation>,
    pub(super) directories: ProcessDirectories,
}

impl Manager {
    /// What the space `home` gives a process that joins one of its
    /// executions, as it gives a new execution's command: its configured
    /// environment, and the ports it reserved.
    pub(crate) async fn joining_environment(
        &self,
        home: Home,
    ) -> Result<(Home, BTreeMap<String, OsString>, Vec<PortReservation>)> {
        let caches = home.caches();
        let (home, provisions) = self
            .store
            .write(move |tx| {
                let provisions = home.provide(tx, &caches)?;
                Ok((home, provisions))
            })
            .await?;
        let environment = home.environment(&provisions.ports, &provisions.directories);
        Ok((home, environment, provisions.ports))
    }

    /// The space `id` as a place to run an execution: a workspace whose
    /// worktree is there and verified to be its own, or a chat whose home
    /// is there. Running there counts as the space's activity.
    pub(crate) async fn execution_home(&self, id: &SpaceId) -> Result<Home> {
        let space = self.space_by_id(id).await?;
        let home = match space.by_kind() {
            ByKind::Workspace(workspace) => {
                ensure!(
                    off_runtime::is_dir(&workspace.path).await?,
                    "workspace directory is missing"
                );
                self.verify_worktree(&workspace).await?;
                let settings = self.workspace_settings(&workspace).await?;
                let repository = self.repositories.repository(workspace.repository()).await?;
                Home::Workspace {
                    workspace,
                    repository,
                    settings,
                }
            }
            ByKind::Chat(chat) => self.chat_execution_home(chat).await?,
        };
        self.touch(id).await;
        Ok(home)
    }

    /// The chat `chat` as a place to run its agent, or a process an
    /// external caller starts there: its home, which must be there, its
    /// settings and its temporary directory.
    pub(crate) async fn chat_execution_home(&self, chat: Chat) -> Result<Home> {
        let path = self.chat_path(&chat).await?;
        ensure!(
            self.file_access
                .checked(&self.notifications, &path, off_runtime::is_dir(&path))
                .await?,
            "the directory of chat {} is missing at {}",
            chat.name,
            path.display()
        );
        let repository = self.chat_repository(&chat).await?;
        let tmp_dir = self.chat_tmp_dir(repository.as_ref(), &chat)?;
        let settings = self.space_settings(&Space::from(chat.clone())).await?;
        Ok(Home::Chat {
            chat,
            repository,
            settings,
            path,
            tmp_dir,
            removing: self.space_removals.clone(),
        })
    }

    /// The directory the agents of `space` run in, which tells their
    /// processes apart: a workspace's worktree, or a chat's directory,
    /// whether or not a chat could run in the worktree there now.
    pub(crate) async fn space_directory(&self, space: &Space) -> Result<PathBuf> {
        Ok(space.workdir.path().to_owned())
    }

    /// The directory the agent of `chat` runs in: the worktree of the
    /// workspace it runs in, once a chat may run there, else its checkout
    /// or `root_dir`.
    pub(crate) async fn chat_path(&self, chat: &Chat) -> Result<PathBuf> {
        match chat.workdir.workspace() {
            Some(workspace) => {
                let selector = SpaceSelector::id(&workspace.id);
                Ok(self.chat_workspace(&selector).await?.part.path)
            }
            None => self.chat_checkout_path(chat).await,
        }
    }
}

impl Home {
    pub(super) fn id(&self) -> &SpaceId {
        match self {
            Self::Workspace { workspace, .. } => &workspace.id,
            Self::Chat { chat, .. } => &chat.id,
        }
    }

    /// The space as the execution's plan names it.
    pub(crate) fn plan_space(&self) -> ExecutionSpace {
        match self {
            Self::Workspace { workspace, .. } => workspace.into(),
            Self::Chat { chat, path, .. } => ExecutionSpace {
                id: chat.id.clone(),
                number: chat.number,
                kind: crate::model::SpaceKind::Chat,
                path: path.clone(),
            },
        }
    }

    /// The space as the execution's connection keeps it.
    pub(super) fn into_space(self) -> Space {
        match self {
            Self::Workspace { workspace, .. } => Space::from(workspace),
            Self::Chat { chat, .. } => Space::from(chat),
        }
    }

    /// The caches the space's settings configure for the execution's
    /// processes.
    pub(super) fn caches(&self) -> BTreeMap<String, crate::config::repo::Cache> {
        match self {
            Self::Workspace { settings, .. } | Self::Chat { settings, .. } => {
                settings.caches.clone()
            }
        }
    }

    /// Apply the lifecycle checks of an execution of `kind` in the space,
    /// in the transaction that records it: a workspace's kind decides them,
    /// while a chat runs only its one agent, as `agent` says it is, and
    /// none while a removal holds it.
    pub(super) fn reserve(
        &self,
        tx: &Transaction<'_>,
        kind: ExecutionKind,
        agent: bool,
        parent_execution: Option<&ExecutionId>,
    ) -> Result<()> {
        match self {
            Self::Workspace { workspace, .. } => kind.reserve(tx, &workspace.id, parent_execution),
            Self::Chat { chat, removing, .. } => {
                ensure!(
                    kind == ExecutionKind::Command && agent,
                    "a chat runs only its agent; run commands in a workspace"
                );
                ensure!(
                    store::exists(
                        tx,
                        "SELECT 1 FROM spaces WHERE id=?1 AND kind='chat'",
                        [&chat.id]
                    )?,
                    "the chat was removed"
                );
                ensure!(!removing.reserved(&chat.id), "the chat is being removed");
                ensure!(
                    !store::exists(tx, "SELECT 1 FROM executions WHERE space_id=?1", [&chat.id])?,
                    "chat {} already runs its agent",
                    chat.name
                );
                Ok(())
            }
        }
    }

    /// The scope token a new execution's processes carry, read in the
    /// transaction that records it: a new one for a workspace's, and for a
    /// chat's agent the chat's own, which lasts as long as the chat.
    pub(super) fn scope_token(&self, tx: &Transaction<'_>) -> Result<ScopeToken> {
        match self {
            Self::Workspace { .. } => Ok(ScopeToken::generate()),
            Self::Chat { chat, .. } => tokens::agent_token(tx, &chat.id)?.context("no such chat"),
        }
    }

    /// What an execution's plan reads from the space in the transaction
    /// that records it: the workspace's ports, and the directories its
    /// processes use, created when missing; a chat reserves no ports.
    pub(super) fn provide(
        &self,
        tx: &Transaction<'_>,
        caches: &BTreeMap<String, crate::config::repo::Cache>,
    ) -> Result<Provisions> {
        match self {
            Self::Workspace {
                workspace,
                repository,
                ..
            } => Ok(Provisions {
                ports: store::ports(tx, Some(&workspace.id))?,
                directories: ProcessDirectories::prepare(repository, caches, &workspace.id)?,
            }),
            Self::Chat {
                repository,
                tmp_dir,
                ..
            } => Ok(Provisions {
                ports: Vec::new(),
                directories: ProcessDirectories::of_chat(
                    repository.as_ref(),
                    caches,
                    tmp_dir.clone(),
                )?,
            }),
        }
    }

    /// The environment the space gives the execution's processes: its
    /// configured `[env]` with its ports and directories, a chat's without
    /// the variables that name a worktree.
    pub(super) fn environment(
        &self,
        ports: &[PortReservation],
        directories: &ProcessDirectories,
    ) -> BTreeMap<String, OsString> {
        match self {
            Self::Workspace {
                workspace,
                repository,
                settings,
            } => crate::env::render_configured_environment(
                &settings.env,
                workspace,
                Some(repository.display_name()),
                ports,
                &directories.caches,
                directories.tmp_dir.as_deref(),
            ),
            Self::Chat {
                chat,
                repository,
                settings,
                ..
            } => crate::env::render_configured_environment(
                &settings.env,
                chat,
                repository.as_ref().map(Repository::display_name),
                ports,
                &directories.caches,
                directories.tmp_dir.as_deref(),
            ),
        }
    }
}
