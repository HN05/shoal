//! Creating a workspace, or a child split from one, and following its
//! preparation, the setup and what follows it that the daemon runs in an
//! operation: its progress as it comes, then the prepared workspace or how
//! preparing it failed, with the workspace as the failure left it.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context as _, Result};

use crate::{
    connection::{self, Daemon},
    forge::items::reference::ItemInput,
    model::Workspace,
    operations::{Followed, Following},
    protocol::{
        CopySource, CreationOptions, OperationMethod,
        operations::{
            Ended, Failure, LaunchRequest, OperationSpec, Preparation, Prepared, Progress,
            QueuedLaunch, StartedWorkspace, WorkspaceStart,
        },
    },
    queue::QueueEntryId,
    repository::Selector,
    space_names::SpaceSelector,
    state::WorkspaceState,
};

/// A workspace for the daemon to open and prepare in an operation.
pub struct NewWorkspace {
    pub repository: Selector,
    pub start: WorkspaceStart,
    /// The branch a new branch starts from.
    pub base: Option<String>,
    pub path: Option<PathBuf>,
    pub git_profile: Option<String>,
    /// The chat the workspace is created in.
    pub parent: Option<SpaceSelector>,
    pub options: CreationOptions,
    /// The issue to link, by number or URL.
    pub issue: Option<ItemInput>,
    /// The PR to link, by number or URL.
    pub pr: Option<ItemInput>,
    /// The agent its preparation queues, which the daemon resolves under
    /// the workspace's settings.
    pub agent: Option<Box<LaunchRequest>>,
    /// The starting queue entry whose launch opens it.
    pub queue_entry: Option<QueueEntryId>,
    /// The agent the caller starts itself once setup succeeds, which waits
    /// in the queue should setup fail.
    pub waiting: Option<Box<QueuedLaunch>>,
}

impl NewWorkspace {
    /// A workspace of `repository` that starts at `start`, with defaults
    /// for everything else.
    pub fn new(repository: Selector, start: WorkspaceStart) -> Self {
        Self {
            repository,
            start,
            base: None,
            path: None,
            git_profile: None,
            parent: None,
            options: CreationOptions::default(),
            issue: None,
            pr: None,
            agent: None,
            queue_entry: None,
            waiting: None,
        }
    }

    /// The request that opens it and starts preparing it with `env`.
    fn method(self, env: BTreeMap<String, String>) -> OperationMethod {
        let Self {
            repository,
            start,
            base,
            path,
            git_profile,
            parent,
            options,
            issue,
            pr,
            agent,
            queue_entry,
            waiting,
        } = self;
        OperationMethod::StartWorkspace {
            repository,
            start,
            base,
            path,
            git_profile,
            parent,
            options,
            issue,
            pr,
            agent,
            waiting,
            env: Some(env),
            queue_entry,
        }
    }
}

/// A child workspace for the daemon to split and prepare in an operation.
pub struct NewSplit {
    /// The workspace it is split from.
    pub workspace: SpaceSelector,
    /// Its new branch; a copy's defaults to one named after the copied
    /// workspace's.
    pub name: Option<String>,
    /// The workspace it copies, instead of starting from `base`.
    pub copy: Option<CopySource>,
    /// The branch its branch starts from.
    pub base: Option<String>,
    pub options: CreationOptions,
    /// The issue to link, by number or URL.
    pub issue: Option<ItemInput>,
    /// The agent its preparation queues, which the daemon resolves under
    /// the workspace's settings.
    pub agent: Option<Box<LaunchRequest>>,
}

impl NewSplit {
    /// A child of `workspace` on a new branch `name`, with defaults for
    /// everything else.
    pub fn new(workspace: SpaceSelector, name: Option<String>) -> Self {
        Self {
            workspace,
            name,
            copy: None,
            base: None,
            options: CreationOptions::default(),
            issue: None,
            agent: None,
        }
    }

    /// The request that splits it and starts preparing it with `env`.
    fn method(self, env: BTreeMap<String, String>) -> OperationMethod {
        let Self {
            workspace,
            name,
            copy,
            base,
            options,
            issue,
            agent,
        } = self;
        OperationMethod::StartSplit {
            workspace,
            name,
            copy,
            base,
            options,
            issue,
            agent,
            env: Some(env),
        }
    }
}

/// A workspace to create: a new one, or a child split from another.
pub enum Creation {
    Workspace(NewWorkspace),
    Split(NewSplit),
}

impl From<NewWorkspace> for Creation {
    fn from(new: NewWorkspace) -> Self {
        Self::Workspace(new)
    }
}

impl From<NewSplit> for Creation {
    fn from(split: NewSplit) -> Self {
        Self::Split(split)
    }
}

/// Create the workspace `creation` describes and follow its preparation,
/// which runs with `env`.
pub async fn create<'d, D: Daemon>(
    daemon: &'d D,
    creation: impl Into<Creation>,
    env: BTreeMap<String, String>,
) -> Result<Preparing<'d, D>> {
    let method = match creation.into() {
        Creation::Workspace(new) => new.method(env),
        Creation::Split(split) => split.method(env),
    };
    let started = connection::request(daemon, method).await?;
    Preparing::follow(daemon, started).await
}

/// Run `steps` on `workspace` with `env` and follow them.
pub async fn prepare<'d, D: Daemon>(
    daemon: &'d D,
    workspace: &Workspace,
    steps: Preparation,
    env: BTreeMap<String, String>,
) -> Result<Preparing<'d, D>> {
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps,
    };
    let operation = crate::operations::start(daemon, spec, env).await?;
    let started = StartedWorkspace {
        operation,
        workspace: Box::new(workspace.clone()),
        reused: true,
    };
    Preparing::follow(daemon, started).await
}

/// A workspace's preparation being followed.
pub struct Preparing<'d, D> {
    daemon: &'d D,
    workspace: Box<Workspace>,
    reused: bool,
    operation: String,
    following: Following,
    /// The failure it ended with, kept until the workspace it left is read.
    failed: Option<Failure>,
}

impl<'d, D: Daemon> Preparing<'d, D> {
    /// Follow the preparation the daemon `started` for the workspace a
    /// request opened.
    pub async fn follow(daemon: &'d D, started: StartedWorkspace) -> Result<Self> {
        let StartedWorkspace {
            operation,
            workspace,
            reused,
        } = started;
        let following = Following::open(daemon, operation.clone(), 0).await?;
        Ok(Self {
            daemon,
            workspace,
            reused,
            operation,
            following,
            failed: None,
        })
    }

    /// The workspace as it was before its preparation.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Whether the workspace existed before, so it was reopened rather than
    /// created.
    pub fn reused(&self) -> bool {
        self.reused
    }

    /// The preparation's operation, to cancel it.
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// The next step or line its processes wrote, or how it ended. Cancelling
    /// it, as `tokio::select!` does, loses nothing.
    pub async fn next(&mut self) -> Result<Followed<Result<Prepared, Unprepared>>> {
        if self.failed.is_none() {
            match self.following.next().await? {
                Followed::Event(event) => return Ok(Followed::Event(event)),
                Followed::Ended(Ended::Succeeded(outcome)) => {
                    return Ok(Followed::Ended(Ok(Prepared::try_from(*outcome)?)));
                }
                Followed::Ended(Ended::Failed(failure)) => self.failed = Some(failure),
            }
        }
        Ok(Followed::Ended(Err(self.unprepared().await?)))
    }

    /// Follow it to its end, passing each step and line its processes wrote
    /// to `progress`.
    pub async fn finish(
        mut self,
        mut progress: impl FnMut(Progress),
    ) -> Result<Result<Prepared, Unprepared>> {
        loop {
            match self.next().await? {
                Followed::Event(event) => progress(event.progress),
                Followed::Ended(ended) => return Ok(ended),
            }
        }
    }

    /// The failure it ended with, with the workspace as the failure left it.
    async fn unprepared(&mut self) -> Result<Unprepared> {
        let selector = SpaceSelector::id(&self.workspace.id);
        let workspace = crate::workspaces::inspect(self.daemon, selector).await?;
        let failure = self.failed.take().context("the preparation did not fail")?;
        Ok(Unprepared {
            workspace: Box::new(workspace.workspace),
            failure,
        })
    }
}

/// A preparation that failed, with the workspace as the failure left it.
#[derive(Debug)]
pub struct Unprepared {
    pub workspace: Box<Workspace>,
    pub failure: Failure,
}

impl Unprepared {
    /// Whether setup was done before the failure, which came after it, as
    /// in the post-setup hook, leaving the workspace ready. Otherwise its
    /// setup waits for a retry or a skip, or the workspace for removal.
    pub fn set_up(&self) -> bool {
        self.workspace.state == WorkspaceState::Ready
    }
}
