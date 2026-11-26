//! Where a new space goes, which decides its kind, and the step that records
//! it there.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, ensure};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use super::NewSpace;
use crate::{
    config::{Effective, modes::ModeName},
    model::{ChatHome, OpenedWorkspace, Space, SpaceKind, Workspace},
    protocol::{
        CopySource, CreationOptions,
        operations::{QueuedLaunch, WorkspaceStart},
    },
    repository::Selector,
    {
        chats::NewChat,
        scope::Caller,
        workspace::{CopyPlace, NewWorkspace},
    },
};

pub enum Home<'a> {
    Checkout(RepositoryId),
    /// A chat in the worktree of a ready workspace, in its repository.
    Worktree(SpaceSelector),
    Root,
    /// A new workspace, or the one on its branch, its PR head or the record
    /// it names.
    Opening(Selector, Box<Opening<'a>>),
    /// The child workspace a space creates below it, with its worktree.
    Child(RepositoryId, Child),
}

pub struct Opening<'a> {
    pub start: WorkspaceStart,
    pub base: Option<String>,
    pub new: NewWorkspace<'a>,
    pub parent: Option<SpaceId>,
}

/// On the new branch `name` from `base`, or a copy of `copy`, on `name` or a
/// branch named after the copied one's.
pub struct Child {
    pub name: Option<String>,
    pub copy: Option<CopySource>,
    pub base: Option<String>,
    pub options: CreationOptions,
}

/// `reused` where the start reopened a workspace. Also says whether its
/// agent waits for the preparation that follows, as a new workspace's does.
pub struct Recorded {
    pub space: Space,
    pub reused: bool,
    pub awaits_preparation: bool,
}

impl Home<'_> {
    pub(super) fn kind(&self) -> SpaceKind {
        match self {
            Self::Checkout(_) | Self::Worktree(_) | Self::Root => SpaceKind::Chat,
            Self::Opening(..) | Self::Child(..) => SpaceKind::Workspace,
        }
    }
}

impl Spaces {
    pub async fn home_repository(
        &self,
        daemon: &dyn Daemon,
        home: &Home<'_>,
    ) -> Result<Option<RepositoryId>> {
        Ok(match home {
            Home::Checkout(repository) | Home::Child(repository, _) => Some(repository.clone()),
            Home::Worktree(workspace) => {
                self.chat_workspace(daemon, workspace).await?.repository_id
            }
            Home::Root => None,
            Home::Opening(repository, _) => Some(daemon.repository(repository.clone()).await?.id),
        })
    }

    /// Those of the workspace an opening names, its worktree's file
    /// included, or of the parent a child goes below, else the
    /// repository's. For a chat without a repository the global config's.
    /// With the mode files beside them.
    pub(super) async fn home_settings(
        &self,
        daemon: &dyn Daemon,
        home: &Home<'_>,
        repository: Option<&RepositoryId>,
        parent: Option<&SpaceId>,
    ) -> Result<Effective> {
        match (home, repository) {
            (Home::Opening(_, opening), _) => {
                if let WorkspaceStart::Workspace(workspace) = &opening.start {
                    let workspace = self.workspace(workspace).await?;
                    return daemon.workspace_settings_with_templates(&workspace).await;
                }
            }
            (Home::Child(..), _) => {
                let parent = self.space_by_id(parent.context("a child names its parent")?);
                return (daemon.space_settings_with_templates(&parent.await?, repository)).await;
            }
            _ => {}
        }
        match repository {
            Some(repository) => daemon.repository_settings_with_templates(repository).await,
            None => (self.config.current())
                .resolve_with_template_files(&Default::default(), &self.paths),
        }
    }

    /// A chat's is after where it runs; a workspace takes its branch's.
    pub(super) async fn home_name(
        &self,
        daemon: &dyn Daemon,
        home: &Home<'_>,
        mode: &ModeName,
    ) -> Result<Option<SpaceName>> {
        let name = match home {
            Home::Worktree(workspace) => {
                let workspace = self.chat_workspace(daemon, workspace).await?;
                ChatHome::Workspace(&workspace).chat_name(mode)
            }
            Home::Checkout(repository) => {
                let repository = daemon
                    .repository(crate::repository::Selector::from(repository.clone()))
                    .await?;
                ChatHome::Checkout(&repository).chat_name(mode)
            }
            Home::Root => {
                let root_dir = self.config.current().root_dir(&self.paths)?;
                ChatHome::Root(&root_dir).chat_name(mode)
            }
            Home::Opening(..) | Home::Child(..) => return Ok(None),
        };
        Ok(Some(SpaceName::derive(&name, "chat")))
    }

    /// A chat records `agent` with it, and is refused without one. A scoped
    /// `caller` takes over no workspace outside its scope.
    pub(super) async fn record_space(
        &self,
        daemon: &dyn Daemon,
        home: Home<'_>,
        new: NewSpace,
        agent: Option<&QueuedLaunch>,
        caller: Option<&Caller>,
    ) -> Result<Recorded> {
        let NewSpace { name, parent, .. } = new;
        match home {
            Home::Opening(repository, mut opening) => {
                opening.new.options.name = name;
                let opened = self.open_for_start(daemon, repository, *opening, parent, caller);
                Ok(opened.await?.into())
            }
            Home::Child(repository, mut child) => {
                child.options.name = name;
                let parent = parent.context("a child names its parent")?;
                let child = self
                    .record_child(daemon, &parent, &repository, child)
                    .await?;
                Ok(Recorded {
                    space: child.into(),
                    reused: false,
                    awaits_preparation: true,
                })
            }
            chat => {
                self.record_home_chat(daemon, &chat, (name, parent), agent)
                    .await
            }
        }
    }

    /// The parent must take a chat of its repository.
    async fn record_home_chat(
        &self,
        daemon: &dyn Daemon,
        home: &Home<'_>,
        (name, parent): (Option<SpaceName>, Option<SpaceId>),
        agent: Option<&QueuedLaunch>,
    ) -> Result<Recorded> {
        let agent = agent.context("a chat starts with an agent")?;
        let repository = self
            .home_repository(daemon, home)
            .await?
            .map(Selector::from);
        (self.ensure_parent_takes(daemon, repository.as_ref(), parent.as_ref())).await?;
        let new = NewChat {
            name: name.context("a chat is named")?,
            agent: agent.agent.clone(),
            mode: (agent.mode.clone()).unwrap_or(ModeName::DEFAULT),
            narrowing: agent.narrowing,
            origin: None,
            parent,
            priority: None,
        };
        let (repository, workspace) = match home {
            Home::Worktree(workspace) => (None, Some(workspace)),
            _ => (repository, None),
        };
        let chat = (self.start_new_chat(daemon, repository, workspace, new)).await?;
        Ok(Recorded {
            space: chat.into(),
            reused: false,
            awaits_preparation: false,
        })
    }

    async fn record_child(
        &self,
        daemon: &dyn Daemon,
        parent: &SpaceId,
        repository: &RepositoryId,
        Child {
            name,
            copy,
            base,
            options,
        }: Child,
    ) -> Result<Workspace> {
        let Some(copy) = copy else {
            let name = name.context("name the child's branch")?;
            return self
                .create_child(daemon, &SpaceSelector::id(parent), name, base, options)
                .await;
        };
        ensure!(base.is_none(), "a copy starts from its source, not a base");
        let new = NewWorkspace {
            options,
            parent: Some(parent.clone()),
            ..Default::default()
        };
        let repository = crate::repository::Selector::from(repository.clone());
        (self.create_copy(daemon, repository, &copy, CopyPlace::Branch(name), new)).await
    }

    /// A new or existing branch gets a worktree its preparation creates. A
    /// workspace on that branch or its PR head, or the recorded one it
    /// names, is opened instead and keeps its place. A scoped `caller` takes
    /// over no workspace outside its subtree.
    async fn open_for_start(
        &self,
        daemon: &dyn Daemon,
        repository: Selector,
        Opening {
            start, base, new, ..
        }: Opening<'_>,
        parent: Option<SpaceId>,
        caller: Option<&Caller>,
    ) -> Result<OpenedWorkspace> {
        let placed = base.is_some() || new.path.is_some() || new.git_profile.is_some();
        // A new worktree waits for the preparation the start runs.
        let new = NewWorkspace {
            pending: !matches!(start, WorkspaceStart::Workspace(_)),
            ..new
        };
        match start {
            WorkspaceStart::Name(name) => {
                let workspace = self
                    .create_workspace_below(daemon, repository, name, base, new, parent)
                    .await?;
                Ok(OpenedWorkspace {
                    workspace,
                    reused: false,
                })
            }
            WorkspaceStart::Typed(name) => {
                let new = NewWorkspace {
                    exact_branch: true,
                    ..new
                };
                let workspace = self
                    .create_workspace_below(daemon, repository, name, base, new, parent)
                    .await?;
                Ok(OpenedWorkspace {
                    workspace,
                    reused: false,
                })
            }
            WorkspaceStart::Branch(branch) => {
                self.open_branch_below(daemon, repository, &branch, base, new, parent, caller)
                    .await
            }
            WorkspaceStart::Copy {
                source,
                branch,
                typed,
            } => {
                ensure!(base.is_none(), "a copy starts from its source, not a base");
                let new = NewWorkspace {
                    parent: parent.clone(),
                    exact_branch: typed,
                    ..new
                };
                let place = CopyPlace::Branch(branch);
                let workspace = self
                    .create_copy(daemon, repository, &source, place, new)
                    .await?;
                Ok(OpenedWorkspace {
                    workspace: self
                        .give_to_parent(daemon, &SpaceSelector::id(&workspace.id), parent)
                        .await?,
                    reused: false,
                })
            }
            WorkspaceStart::PullHead { url, head } => {
                ensure!(
                    !placed,
                    "a workspace at a PR's head takes no base, path or Git profile"
                );
                let new = NewWorkspace { parent, ..new };
                (self.open_pull_head(daemon, repository, url, head, new, caller)).await
            }
            WorkspaceStart::Workspace(workspace) => {
                ensure!(
                    !placed && parent.is_none(),
                    "an existing workspace takes no base, path, Git profile or parent"
                );
                let repository = daemon.repository(repository).await?;
                let workspace = self.workspace(&workspace).await?;
                ensure!(
                    workspace.repository() == &repository.id,
                    "workspace {} is not in the requested repository",
                    workspace.name
                );
                self.reopen_workspace(daemon, &SpaceSelector::id(&workspace.id))
                    .await
            }
        }
    }
}

impl From<OpenedWorkspace> for Recorded {
    fn from(OpenedWorkspace { workspace, reused }: OpenedWorkspace) -> Self {
        Self {
            space: workspace.into(),
            reused,
            awaits_preparation: !reused,
        }
    }
}
