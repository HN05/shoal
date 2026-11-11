//! The agent of a workspace whose setup failed: it waits in the agent queue
//! for a retried setup, without the slot its start held, unless the user
//! retries or ignores the failure at once, which takes it back to start as
//! it would have.
use anyhow::Result;

use super::prepare::queue_in_workspace;
use crate::client::queue::QueueEntryId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{commands::queued_agents, context::Context},
    model::{QueueEntry, Workspace},
    protocol::{ConfigTarget, operations::QueuedLaunch},
    state::WorkspaceState,
};

pub(in crate::cli::commands) struct WaitingAgent {
    launch: QueuedLaunch,
    /// The starting queue entry whose slot the launch holds, until a failed
    /// setup frees it.
    pub starting: Option<QueueEntryId>,
    /// Its entry while it waits in the queue.
    queued: Option<QueueEntry>,
}

impl WaitingAgent {
    /// The agent `launch` would start, holding the slot of `starting`.
    pub(in crate::cli::commands) fn new(
        launch: QueuedLaunch,
        starting: Option<QueueEntryId>,
    ) -> Self {
        Self {
            launch,
            starting,
            queued: None,
        }
    }

    /// Free the starting slot and queue the agent in `workspace`, if its
    /// setup failed, to start once a retried setup readies it, unless it
    /// waits there already.
    pub(super) async fn park(&mut self, ctx: &Context, workspace: &Workspace) {
        if self.queued.is_some() || !setup_failed(ctx, workspace).await {
            return;
        }
        let (name, number) = (&workspace.name, workspace.number);
        if let Some(entry) = self.starting.take() {
            let error = anyhow::anyhow!("setup of {name} did not complete");
            queued_agents::fail(ctx, &entry, &error).await;
        }
        match queue_waiting(ctx, workspace, self.launch.clone()).await {
            Ok(entry) => {
                eprintln!(
                    "{} waits for the setup of {name}; `shoal resume {number}` retries it",
                    entry.agent
                );
                self.queued = Some(entry);
            }
            Err(error) => {
                eprintln!("warning: the agent of {name} cannot wait for its setup: {error:#}");
            }
        }
    }

    /// Take the agent back from the queue, for the caller to start.
    pub(super) async fn withdraw(&mut self, ctx: &Context) -> Result<()> {
        if let Some(entry) = self.queued.take() {
            crate::client::queue::remove(&ctx.paths, entry.id.to_string()).await?;
        }
        Ok(())
    }
}

/// Whether `workspace`'s setup failed.
async fn setup_failed(ctx: &Context, workspace: &Workspace) -> bool {
    let selector = SpaceSelector::id(&workspace.id);
    crate::client::workspaces::inspect(&ctx.paths, selector)
        .await
        .is_ok_and(|inspection| inspection.workspace.state == WorkspaceState::Failed)
}

/// Queue `launch` in `workspace`, whose setup failed, to wait for it.
async fn queue_waiting(
    ctx: &Context,
    workspace: &Workspace,
    launch: QueuedLaunch,
) -> Result<QueueEntry> {
    let target = ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
    queue_in_workspace(ctx, workspace, &settings, launch).await
}
