//! Creating a workspace, or a child of a space, and following its
//! preparation, the setup and what follows it that the daemon runs in an
//! operation: its progress as it comes, then the prepared workspace or how
//! preparing it failed, with the workspace as the failure left it.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context as _, Result};

use crate::{
    connection::{self, Daemon},
    forge::items::reference::ItemInput,
    operations::{Followed, Following},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::{
    model::{Blocker, QueueEntry, Workspace},
    protocol::{
        CopySource, CreationOptions, OperationMethod,
        operations::{
            Ended, Failure, LaunchRequest, OperationSpec, Preparation, Prepared, Progress,
            QueuedLaunch, StartedWorkspace, WorkspaceStart,
        },
    },
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
    /// What `agent` waits for besides the preparation, which waits for it
    /// too before it creates a new worktree, on the branch of the done
    /// blocker that holds the others' work.
    pub blockers: Vec<Blocker>,
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
            blockers: Vec::new(),
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
            blockers,
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
            blockers,
            env: Some(env),
            queue_entry,
        }
    }
}

/// A child workspace for the daemon to create and prepare in an operation.
pub struct NewChild {
    /// The space it goes below, a workspace or a chat.
    pub parent: SpaceSelector,
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
    /// the parent's settings.
    pub agent: Option<Box<LaunchRequest>>,
}

impl NewChild {
    /// A child of the space `parent` on a new branch `name`, with defaults
    /// for everything else.
    pub fn new(parent: SpaceSelector, name: Option<String>) -> Self {
        Self {
            parent,
            name,
            copy: None,
            base: None,
            options: CreationOptions::default(),
            issue: None,
            agent: None,
        }
    }

    /// The request that creates it and starts preparing it with `env`.
    fn method(self, env: BTreeMap<String, String>) -> OperationMethod {
        let Self {
            parent,
            name,
            copy,
            base,
            options,
            issue,
            agent,
        } = self;
        OperationMethod::StartChild {
            parent,
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

/// A workspace to create: a new one, a child of a space, or the one a
/// start without an agent describes, which the daemon places as a launch's.
pub enum Creation {
    Workspace(NewWorkspace),
    Child(NewChild),
    Placed(Box<shoal_protocol::launches::Launch>),
}

impl From<NewWorkspace> for Creation {
    fn from(new: NewWorkspace) -> Self {
        Self::Workspace(new)
    }
}

impl From<shoal_protocol::launches::Launch> for Creation {
    fn from(launch: shoal_protocol::launches::Launch) -> Self {
        Self::Placed(Box::new(launch))
    }
}

impl From<NewChild> for Creation {
    fn from(child: NewChild) -> Self {
        Self::Child(child)
    }
}

/// Create the workspace `creation` describes and follow its preparation,
/// which runs with `env`.
pub async fn create<'d, D: Daemon>(
    daemon: &'d D,
    creation: impl Into<Creation>,
    env: BTreeMap<String, String>,
) -> Result<Preparing<'d, D>> {
    let started = start(daemon, creation, env).await?;
    Preparing::follow(daemon, started).await
}

/// Create the workspace `creation` describes and start its preparation,
/// which runs with `env`, without following it, as for one whose
/// preparation waits for its agent's blockers.
pub async fn start<D: Daemon>(
    daemon: &D,
    creation: impl Into<Creation>,
    env: BTreeMap<String, String>,
) -> Result<StartedWorkspace> {
    let method = match creation.into() {
        Creation::Workspace(new) => new.method(env),
        Creation::Child(child) => child.method(env),
        Creation::Placed(launch) => OperationMethod::StartCreation {
            launch,
            env: Some(env),
        },
    };
    connection::request(daemon, method).await
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
        operation: Some(operation),
        workspace: Box::new(workspace.clone()),
        reused: true,
        entry: None,
        warnings: Vec::new(),
    };
    Preparing::follow(daemon, started).await
}

/// A workspace's preparation being followed.
pub struct Preparing<'d, D> {
    daemon: &'d D,
    workspace: Box<Workspace>,
    reused: bool,
    /// The queue entry of the agent the start queued there, which waits for
    /// the preparation.
    entry: Option<Box<QueueEntry>>,
    /// What assigning its issue on the forge warned of.
    warnings: Vec<String>,
    operation: String,
    following: Following,
    /// The failure it ended with, kept until the workspace it left is read.
    failed: Option<Failure>,
}

impl<'d, D: Daemon> Preparing<'d, D> {
    /// Follow the preparation the daemon `started` for the workspace a
    /// request opened, refusing one that waits, which has nothing to follow
    /// yet.
    pub async fn follow(daemon: &'d D, started: StartedWorkspace) -> Result<Self> {
        let StartedWorkspace {
            operation,
            workspace,
            reused,
            entry,
            warnings,
        } = started;
        let operation = operation.with_context(|| {
            let waits = workspace.preparation_waits.as_ref();
            let waits = waits.map_or_else(|| "its turn".to_owned(), ToString::to_string);
            format!("the preparation of {} waits for {waits}", workspace.name)
        })?;
        let following = Following::open(daemon, operation.clone(), 0).await?;
        Ok(Self {
            daemon,
            workspace,
            reused,
            entry,
            warnings,
            operation,
            following,
            failed: None,
        })
    }

    /// The workspace as it was before its preparation.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// The queue entry of the agent the start queued in the workspace,
    /// which waits for its preparation.
    pub fn entry(&self) -> Option<&QueueEntry> {
        self.entry.as_deref()
    }

    /// Whether the workspace existed before, so it was reopened rather than
    /// created.
    pub fn reused(&self) -> bool {
        self.reused
    }

    /// What assigning the workspace's issue on the forge warned of.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
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

#[cfg(test)]
mod tests;
