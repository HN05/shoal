//! A launch that starts the worker of an existing workspace: its own agent,
//! started as a new workspace's is, with no space created.
use anyhow::{Context as _, Result};
use shoal_protocol::launches::{Launch, LaunchSource, QueuedWorker, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use super::shape::Shape;
use crate::{
    daemon::{
        scope::{Caller, ScopeRefusal},
        workspace::Manager,
    },
    forge::items::reference::ItemInput,
    model::{Blocker, Workspace},
    protocol::operations::LaunchRequest,
};

impl Manager {
    /// Queue the worker `launch`, which `source` made, starts in `shape` in
    /// the workspace it names, on the workspace's issue unless it names a
    /// task, once what blocks it is done. A launch that names no agent
    /// starts none, as a start without a terminal to pick one does.
    pub(super) async fn launch_worker(
        &self,
        launch: Launch,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<QueuedWorker> {
        let mut selector = (launch.in_workspace.as_ref())
            .and_then(SpaceChoice::named)
            .cloned()
            .context("the launch names no workspace")?;
        let workspace = self.workspace(&selector).await?;
        let request = worker_request(launch, shape, source);
        if request.launch.agent.is_none() {
            return Ok(QueuedWorker {
                workspace,
                entry: None,
            });
        }
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
        Ok(QueuedWorker {
            workspace,
            entry: Some(entry),
        })
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

/// A worker's launch, with what the launch names as blocking it.
struct WorkerRequest {
    launch: LaunchRequest,
    blockers: Vec<String>,
}

/// The worker `launch` asks for in `shape`, which `source` made.
fn worker_request(launch: Launch, shape: Shape, source: LaunchSource) -> WorkerRequest {
    let Launch {
        agent,
        model,
        fork,
        permissions,
        now,
        here,
        task,
        prompt,
        env,
        blockers,
        ..
    } = launch;
    WorkerRequest {
        launch: LaunchRequest {
            mode: Some(shape.mode),
            narrowing: permissions,
            model,
            args: task,
            prompt,
            fork,
            env,
            now,
            here,
            source: Some(source),
            ..LaunchRequest::new(agent)
        },
        blockers,
    }
}
