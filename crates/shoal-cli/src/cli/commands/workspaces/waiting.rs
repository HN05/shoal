//! The agent of a start whose setup failed: its preparation left it waiting
//! in the agent queue for a retried setup, and the queue starts it once the
//! setup succeeds, so the start frees the slot it held.
use crate::client::queue::QueueEntryId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{commands::queued_agents, context::Context},
    model::Workspace,
    state::WorkspaceState,
};

pub(in crate::cli::commands) struct WaitingAgent {
    /// The starting queue entry whose slot the start holds, until a failed
    /// setup frees it.
    pub starting: Option<QueueEntryId>,
    /// A setup failed, so the agent waits in the queue.
    failed: bool,
}

impl WaitingAgent {
    /// The agent of a start that holds the slot of `starting`.
    pub(in crate::cli::commands) fn new(starting: Option<QueueEntryId>) -> Self {
        Self {
            starting,
            failed: false,
        }
    }

    /// Whether a failed setup left the agent waiting in the queue.
    pub(in crate::cli::commands) fn failed(&self) -> bool {
        self.failed
    }

    /// Note that the agent waits in the queue of `workspace`, if its setup
    /// failed, and free the starting slot.
    pub(super) async fn park(&mut self, ctx: &Context, workspace: &Workspace) {
        if self.failed || !setup_failed(ctx, workspace).await {
            return;
        }
        self.failed = true;
        if let Some(entry) = self.starting.take() {
            let error = anyhow::anyhow!("setup of {} did not complete", workspace.name);
            queued_agents::fail(ctx, &entry, &error).await;
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
