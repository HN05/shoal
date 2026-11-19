//! A launch that starts the worker of an existing workspace: its own agent,
//! started as a new workspace's is, with no space created.
use anyhow::{Context as _, Result};
use shoal_protocol::launches::{Launch, LaunchSource, SpaceChoice};
use shoal_protocol::progress::Blocker;
use shoal_protocol::protocol::{Launched, Started};
use shoal_protocol::space_names::SpaceSelector;

use super::shape::Shape;
use crate::{
    daemon::{
        manager::Manager,
        scope::{Caller, ScopeRefusal},
    },
    forge::items::reference::ItemInput,
    model::Workspace,
    protocol::operations::LaunchRequest,
};

impl Manager {
    /// Queue the worker `launch`, which `source` made, starts in `shape` in
    /// the workspace it names, on the workspace's issue unless it names a
    /// task, once what blocks it is done: the agent it names, else
    /// `agent.default`, refused without either.
    pub(super) async fn launch_worker(
        &self,
        launch: Launch,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        let mut selector = (launch.in_workspace.as_ref())
            .and_then(SpaceChoice::named)
            .cloned()
            .context("the launch names no workspace")?;
        let workspace = self.workspace(&selector).await?;
        let request = worker_request(launch, shape, source);
        if let Some(caller) = caller {
            let allowed = self
                .ensure_worker_allowed(caller, &mut selector, &request)
                .await;
            ScopeRefusal::of(allowed)?;
        }
        let blockers = self.worker_blockers(&workspace, &request, caller).await?;
        let entry = self
            .queue_agent(&selector, request.launch, blockers, None)
            .await?;
        Ok(queued(workspace, entry))
    }

    /// Refuse what starting the worker `launch` names in `shape` for
    /// `caller` would refuse, as [`Self::launch_worker`] starts it, short
    /// of queuing it: a workspace outside a scoped caller's tree,
    /// permissions it does not hold, and blockers no one knows; returns
    /// the workspace.
    pub(super) async fn check_worker(
        &self,
        launch: &Launch,
        shape: &Shape,
        caller: Option<&Caller>,
    ) -> Result<Workspace> {
        let mut selector = (launch.in_workspace.as_ref())
            .and_then(SpaceChoice::named)
            .cloned()
            .context("the launch names no workspace")?;
        if let Some(caller) = caller {
            let (mode, given) = (Some(&shape.mode), &launch.permissions);
            self.worker_target(caller, &mut selector, mode, given)
                .await?;
        }
        let workspace = self.workspace(&selector).await?;
        let repository = workspace.repository();
        (self.named_blockers(repository, &launch.blockers, caller)).await?;
        Ok(workspace)
    }

    /// Refuse the scoped `caller` a worker it may not start in the
    /// workspace `selector` names, or start regardless of the limits.
    async fn ensure_worker_allowed(
        &self,
        caller: &Caller,
        selector: &mut SpaceSelector,
        request: &WorkerRequest,
    ) -> Result<()> {
        let launch = &request.launch;
        let (mode, given) = (launch.mode.as_ref(), &launch.narrowing);
        let target = self.worker_target(caller, selector, mode, given).await?;
        if launch.now {
            (self.authorize_start_beyond_limits(caller, "start", target)).await?;
        }
        Ok(())
    }

    /// What holds the worker of `request` in `workspace`: what it names,
    /// and, unless it starts now or on a task of its own, the issues the
    /// forge records as blocking the workspace's issue.
    async fn worker_blockers(
        &self,
        workspace: &Workspace,
        request: &WorkerRequest,
        caller: Option<&Caller>,
    ) -> Result<Vec<Blocker>> {
        let repository = workspace.repository();
        let mut blockers = (self.named_blockers(repository, &request.blockers, caller)).await?;
        let on_issue = request.launch.args.is_empty() && request.launch.prompt.is_none();
        if request.launch.now {
            return Ok(Vec::new());
        }
        if let Some(issue) = workspace.links.issue().filter(|_| on_issue) {
            let issue = ItemInput::Url(issue.url.clone());
            blockers.extend(self.forge_blockers(repository, &issue, caller).await?);
        }
        Ok(blockers)
    }
}

/// The launch of the worker of `workspace`, which waits as its queue
/// `entry`.
fn queued(workspace: Workspace, entry: crate::model::QueueEntry) -> Launched {
    Launched {
        space: Box::new(workspace.into()),
        entry: Some(Box::new(entry)),
        warnings: Vec::new(),
        start: Started::Worker,
    }
}

/// A worker's launch, with what the launch names as blocking it.
struct WorkerRequest {
    launch: LaunchRequest,
    blockers: Vec<String>,
}

/// The worker `launch` asks for in `shape`, which `source` made.
fn worker_request(launch: Launch, shape: Shape, source: LaunchSource) -> WorkerRequest {
    WorkerRequest {
        launch: super::space::agent_request(&launch, &shape.mode, source),
        blockers: launch.blockers,
    }
}
