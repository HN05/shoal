//! What an execution takes from the space it runs in, whose records differ
//! by kind: a workspace's verified worktree, ports, caches and configured
//! environment, or a chat's home, its own scope token and its temporary
//! directory. Registration asks nothing else of a space's kind.
use crate::client::execution_ids::ExecutionId;
use crate::client::scope_tokens::ScopeToken;
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, ensure};
use rusqlite::Transaction;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use super::{ExecutionKind, ExecutionKindExt, ProcessDirectories};
use crate::{
    config::Effective,
    daemon::{chats, store, workspace::Manager},
    fsutil::off_runtime,
    model::{Chat, ExecutionSpace, PortReservation, PresentSpace, Repository, Workspace},
};

/// The space an execution registers in, read and checked for its plan.
#[allow(
    clippy::large_enum_variant,
    reason = "read once by the registration that made it"
)]
pub(super) enum Home {
    Workspace {
        workspace: Workspace,
        repository: Repository,
        settings: Effective,
    },
    Chat {
        chat: Chat,
        /// The directory its agent runs in.
        path: PathBuf,
        /// Its temporary directory, created when missing.
        tmp_dir: Option<PathBuf>,
        /// The removals under way, which its registration must not race.
        removing: chats::Removing,
    },
}

/// What an execution's plan reads from its space in the transaction that
/// records it.
pub(super) struct Provisions {
    pub(super) ports: Vec<PortReservation>,
    pub(super) directories: ProcessDirectories,
}

impl Manager {
    /// The space `id` as a place to run an execution: a workspace whose
    /// worktree is there and verified to be its own, or a chat whose home
    /// is there. Running there counts as the space's activity.
    pub(super) async fn execution_home(&self, id: &SpaceId) -> Result<Home> {
        let space = (self.find_space(id).await?).with_context(|| format!("no space {id}"))?;
        let home = match space {
            PresentSpace::Workspace(workspace) => {
                ensure!(
                    off_runtime::is_dir(&workspace.path).await?,
                    "workspace directory is missing"
                );
                self.verify_worktree(&workspace).await?;
                let settings = self.workspace_settings(&workspace).await?;
                let repository = self.repository(&workspace.repository_id).await?;
                Home::Workspace {
                    workspace,
                    repository,
                    settings,
                }
            }
            PresentSpace::Chat(chat) => {
                let path = self.chat_path(&chat).await?;
                ensure!(
                    self.check_access(&path, off_runtime::is_dir(&path)).await?,
                    "the directory of chat {} is missing at {}",
                    chat.name,
                    path.display()
                );
                let repository = self.chat_repository(&chat).await?;
                let tmp_dir = self.chat_tmp_dir(repository.as_ref(), &chat)?;
                Home::Chat {
                    chat,
                    path,
                    tmp_dir,
                    removing: self.chat_removals.clone(),
                }
            }
        };
        self.touch(id).await;
        Ok(home)
    }

    /// The directory the agent of `chat` runs in: the worktree of the
    /// workspace it runs in, else its checkout or `root_dir`.
    pub(crate) async fn chat_path(&self, chat: &Chat) -> Result<PathBuf> {
        match &chat.workspace {
            Some(workspace) => Ok(self.workspace_by_id(&workspace.id).await?.path),
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
    pub(super) fn plan_space(&self) -> ExecutionSpace {
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
    pub(super) fn into_space(self) -> PresentSpace {
        match self {
            Self::Workspace { workspace, .. } => PresentSpace::Workspace(workspace),
            Self::Chat { chat, .. } => PresentSpace::Chat(chat),
        }
    }

    /// The configured caches the execution's processes use: a workspace's;
    /// a chat runs with Shoal's environment.
    pub(super) fn caches(&self) -> BTreeMap<String, crate::config::repo::Cache> {
        match self {
            Self::Workspace { settings, .. } => settings.caches.clone(),
            Self::Chat { .. } => BTreeMap::new(),
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
                    store::exists(tx, "SELECT 1 FROM chats WHERE id=?1", [&chat.id])?,
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
            Self::Workspace { .. } => Ok(crate::daemon::scope::new_token()),
            Self::Chat { chat, .. } => chats::scope_token(tx, &chat.id)?.context("no such chat"),
        }
    }

    /// What an execution's plan reads from the space in the transaction
    /// that records it: the workspace's ports, and the directories its
    /// processes use, created when missing.
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
            Self::Chat { tmp_dir, .. } => Ok(Provisions {
                ports: Vec::new(),
                directories: ProcessDirectories {
                    caches: BTreeMap::new(),
                    tmp_dir: tmp_dir.clone(),
                },
            }),
        }
    }

    /// The environment the space gives the execution's processes: a
    /// workspace's configured `[env]` with its ports and directories, while
    /// a chat runs with Shoal's environment and its temporary directory.
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
                crate::repository::name(repository),
                ports,
                &directories.caches,
                directories.tmp_dir.as_deref(),
            ),
            Self::Chat { .. } => (directories.tmp_dir.iter())
                .map(|directory| (crate::env::TMPDIR.to_owned(), directory.into()))
                .collect(),
        }
    }
}
