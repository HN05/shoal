//! Creating a workspace, or a child split from one: the request that opens
//! it and starts the operation that prepares it.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;

use crate::space_names::SpaceSelector;
use crate::{
    connection::{self, Daemon},
    protocol::{
        CreationOptions, OperationMethod,
        operations::{QueuedLaunch, StartedWorkspace, WorkspaceStart},
    },
    repository::Selector,
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
    /// The issue to link, by URL.
    pub issue: Option<String>,
    /// The PR to link, by URL.
    pub pr: Option<String>,
    /// The agent its preparation queues.
    pub agent: Option<Box<QueuedLaunch>>,
    /// The starting queue entry whose launch opens it.
    pub queue_entry: Option<crate::queue::QueueEntryId>,
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
}

/// Open `workspace` and start preparing it with `env`.
pub async fn start_workspace(
    daemon: &impl Daemon,
    workspace: NewWorkspace,
    env: BTreeMap<String, String>,
) -> Result<StartedWorkspace> {
    let NewWorkspace {
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
    } = workspace;
    let method = OperationMethod::StartWorkspace {
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
    };
    connection::request(daemon, method).await
}

/// A child workspace for the daemon to split and prepare in an operation.
pub struct NewSplit {
    /// The workspace it is split from.
    pub workspace: SpaceSelector,
    /// Its new branch; a copy's defaults to one named after the copied
    /// workspace's.
    pub name: Option<String>,
    /// The workspace it copies, instead of starting from `base`.
    pub copy: Option<crate::protocol::CopySource>,
    /// The branch its branch starts from.
    pub base: Option<String>,
    pub options: CreationOptions,
    /// The issue to link, by URL.
    pub issue: Option<String>,
    /// The agent its preparation queues.
    pub agent: Option<Box<QueuedLaunch>>,
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
}

/// Split `split` and start preparing it with `env`.
pub async fn start_split(
    daemon: &impl Daemon,
    split: NewSplit,
    env: BTreeMap<String, String>,
) -> Result<StartedWorkspace> {
    let NewSplit {
        workspace,
        name,
        copy,
        base,
        options,
        issue,
        agent,
    } = split;
    let method = OperationMethod::StartSplit {
        workspace,
        name,
        copy,
        base,
        options,
        issue,
        agent,
        env: Some(env),
    };
    connection::request(daemon, method).await
}
