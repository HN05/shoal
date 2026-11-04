//! Following an operation, work the daemon runs in a worker of its own, such
//! as preparing a workspace it created: its progress as it comes, then how it
//! ended. The CLI follows its operations the same way.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use tokio::{io::BufReader, net::UnixStream};

use crate::client::{
    connection::{self, Daemon},
    protocol::{
        Body, CreationOptions, OperationMethod, Responses,
        operations::{
            Ended, OperationEvent, OperationSpec, QueuedLaunch, Report, Started, StartedWorkspace,
            WorkspaceStart,
        },
    },
    repository::Selector,
};

/// What following an operation reports.
#[derive(Debug, Clone)]
pub enum Followed {
    /// A step it reached or a line its processes wrote.
    Event(OperationEvent),
    /// How it ended; nothing follows.
    Ended(Ended),
}

/// An operation being followed, on a connection of its own.
pub struct Following {
    stream: Responses<BufReader<UnixStream>>,
}

impl Following {
    /// Follow operation `id` from its `after`th event, failing at once when
    /// the daemon refuses.
    pub async fn open(daemon: &impl Daemon, id: String, after: u64) -> Result<Self> {
        let method = OperationMethod::FollowOperation { id, after };
        let (stream, body) = connection::open_buffered(daemon, method).await?;
        <()>::try_from(body)?;
        Ok(Self {
            stream: Responses::new(stream),
        })
    }

    /// The next event, or the operation's end. Cancelling it, as
    /// `tokio::select!` does, loses no event. A daemon that goes away ends
    /// following with an error.
    pub async fn next(&mut self) -> Result<Followed> {
        let response = match self.stream.next().await {
            Err(error) if connection::stream_closed(&error) => {
                anyhow::bail!("lost the daemon while following the operation, which continues")
            }
            response => response?,
        };
        match response.body.into_result()? {
            Body::OperationEvent(event) => Ok(Followed::Event(event)),
            Body::OperationEnded(ended) => Ok(Followed::Ended(ended)),
            body => Err(body.unexpected("OperationEvent")),
        }
    }
}

/// Start `spec` with `env`, returning the started operation's ID to follow.
pub async fn start(
    daemon: &impl Daemon,
    spec: OperationSpec,
    env: BTreeMap<String, String>,
) -> Result<String> {
    let method = OperationMethod::StartOperation {
        spec,
        env: Some(env),
    };
    let Started { id } = connection::request(daemon, method).await?;
    Ok(id)
}

/// Cancel operation `id`.
pub async fn cancel(daemon: &impl Daemon, id: String) -> Result<()> {
    connection::request(daemon, OperationMethod::CancelOperation { id }).await
}

/// Report a step of operation `id`, or its end, from its worker.
pub async fn report(daemon: &impl Daemon, id: String, report: Report) -> Result<()> {
    connection::request(daemon, OperationMethod::ReportProgress { id, report }).await
}

/// A workspace for the daemon to open and prepare in an operation.
pub struct NewWorkspace {
    pub repository: Selector,
    pub start: WorkspaceStart,
    /// The branch a new branch starts from.
    pub base: Option<String>,
    pub path: Option<PathBuf>,
    pub git_profile: Option<String>,
    /// The chat the workspace is created in.
    pub parent: Option<String>,
    pub options: CreationOptions,
    /// The issue to link, by URL.
    pub issue: Option<String>,
    /// The PR to link, by URL.
    pub pr: Option<String>,
    /// The agent its preparation queues.
    pub agent: Option<Box<QueuedLaunch>>,
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
        env: Some(env),
    };
    connection::request(daemon, method).await
}

/// A child workspace for the daemon to split and prepare in an operation.
pub struct NewSplit {
    /// The workspace it is split from.
    pub workspace: String,
    /// Its new branch.
    pub name: String,
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
    pub fn new(workspace: String, name: String) -> Self {
        Self {
            workspace,
            name,
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
        base,
        options,
        issue,
        agent,
    } = split;
    let method = OperationMethod::StartSplit {
        workspace,
        name,
        base,
        options,
        issue,
        agent,
        env: Some(env),
    };
    connection::request(daemon, method).await
}
