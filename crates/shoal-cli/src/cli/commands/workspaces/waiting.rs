//! The agent of a workspace whose setup failed: it waits in the agent queue
//! for a retried setup, without the slot its start held, and the queue
//! starts it once the setup succeeds.
use crate::client::queue::QueueEntryId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{commands::queued_agents, context::Context},
    model::Workspace,
    protocol::operations::QueuedLaunch,
    state::WorkspaceState,
};

pub(in crate::cli::commands) struct WaitingAgent {
    /// The agent the caller starts itself, which it queues; the preparation
    /// queues the one it would have queued.
    launch: Option<QueuedLaunch>,
    /// The starting queue entry whose slot the launch holds, until a failed
    /// setup frees it.
    pub starting: Option<QueueEntryId>,
    /// The PR the start links, which the caller's agent links once it starts.
    pr: Option<String>,
    /// A setup failed, so the agent waits in the queue.
    failed: bool,
}

impl WaitingAgent {
    /// The agent of a start that holds the slot of `starting` and links
    /// `pr`, `launch` when the caller starts it itself.
    pub(in crate::cli::commands) fn new(
        launch: Option<QueuedLaunch>,
        starting: Option<QueueEntryId>,
        pr: Option<String>,
    ) -> Self {
        Self {
            launch,
            starting,
            pr,
            failed: false,
        }
    }

    /// Whether a failed setup left the agent waiting in the queue.
    pub(in crate::cli::commands) fn failed(&self) -> bool {
        self.failed
    }

    /// Free the starting slot and queue the caller's agent in `workspace`,
    /// if its setup failed, unless it waits there already.
    pub(super) async fn park(&mut self, ctx: &Context, workspace: &Workspace) {
        if self.failed || !setup_failed(ctx, workspace).await {
            return;
        }
        self.failed = true;
        if let Some(entry) = self.starting.take() {
            let error = anyhow::anyhow!("setup of {} did not complete", workspace.name);
            queued_agents::fail(ctx, &entry, &error).await;
        }
        if let Some(launch) = self.launch.take() {
            super::prepare::leave_waiting(ctx, workspace, launch, self.pr.take()).await;
        }
    }
}

/// Whether `workspace`'s setup failed.
async fn setup_failed(ctx: &Context, workspace: &Workspace) -> bool {
    let selector = SpaceSelector::id(&workspace.id);
    crate::client::workspaces::inspect(&ctx.paths, selector)
        .await
        .is_ok_and(|inspection| inspection.workspace.state == WorkspaceState::Failed)
}
