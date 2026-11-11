//! An added workspace whose setup failed keeps its agent: the agent waits in
//! the agent queue for a retried setup, without the slot its start held,
//! unless the user retries or ignores the failure at once.
use anyhow::Result;

use crate::client::queue::QueueEntryId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::{
            operations, queued_agents,
            workspaces::{SetUp, prepare::queue_in_workspace, setup::settle_setup},
        },
        context::Context,
    },
    model::{QueueEntry, Workspace},
    protocol::{
        ConfigTarget,
        operations::{Preparation, Prepared, QueuedLaunch, StartedWorkspace},
    },
    state::WorkspaceState,
};

/// What preparing the added workspace does after setup: linking `pr`, then
/// the post-setup hook, then queuing `agent`.
pub(super) struct AfterSetup {
    pub pr: Option<String>,
    pub agent: Option<Box<QueuedLaunch>>,
    /// The agent that waits for a retried setup should setup fail.
    pub waiting: Option<Box<QueuedLaunch>>,
    /// The starting queue entry whose slot the launch holds, until a failed
    /// setup frees it.
    pub entry: Option<QueueEntryId>,
}

/// Follow the preparation of the started workspace and settle a failed
/// setup, after leaving its agent waiting in the queue; a retry runs what
/// follows setup with it.
pub(super) async fn follow_add_setup(
    ctx: &Context,
    started: StartedWorkspace,
    rest: &mut AfterSetup,
) -> Result<Option<SetUp>> {
    let StartedWorkspace {
        operation,
        workspace,
        reused,
    } = started;
    let prepared = operations::follow_started::<Prepared>(ctx, operation).await;
    let waiting = match prepared {
        Ok(_) => None,
        Err(_) => wait_for_setup(ctx, &workspace, rest).await,
    };
    let retry = Preparation {
        setup: true,
        post_setup: !reused,
        pr: rest.pr.clone(),
        agent: rest.agent.clone(),
    };
    settle_setup(ctx, &workspace, prepared, Some(retry), waiting.as_ref()).await
}

/// Leave `rest.waiting` in the queue of `workspace`, if its setup failed, to
/// start once a retried setup readies it, after freeing the slot of the
/// starting `rest.entry`.
async fn wait_for_setup(
    ctx: &Context,
    workspace: &Workspace,
    rest: &mut AfterSetup,
) -> Option<QueueEntry> {
    let (name, number) = (&workspace.name, workspace.number);
    let selector = SpaceSelector::id(&workspace.id);
    let failed = crate::client::workspaces::inspect(&ctx.paths, selector)
        .await
        .is_ok_and(|inspection| inspection.workspace.state == WorkspaceState::Failed);
    if !failed {
        return None;
    }
    if let Some(entry) = rest.entry.take() {
        let error = anyhow::anyhow!("setup of {name} did not complete");
        queued_agents::fail(ctx, &entry, &error).await;
    }
    let launch = rest.waiting.take()?;
    match queue_waiting(ctx, workspace, *launch).await {
        Ok(entry) => {
            eprintln!(
                "{} waits for the setup of {name}; `shoal resume {number}` retries it",
                entry.agent
            );
            Some(entry)
        }
        Err(error) => {
            eprintln!("warning: the agent of {name} cannot wait for its setup: {error:#}");
            None
        }
    }
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
