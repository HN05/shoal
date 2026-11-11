//! Following the added workspace's setup, whose failure leaves its agent
//! waiting in the queue for a retried setup.
use anyhow::Result;

use crate::cli::{
    commands::{
        operations,
        workspaces::{SetUp, setup::settle_setup, waiting::WaitingAgent},
    },
    context::Context,
};
use crate::protocol::operations::{Preparation, Prepared, QueuedLaunch, StartedWorkspace};

/// What preparing the added workspace does after setup: linking `pr`, then
/// the post-setup hook, then queuing `agent`.
pub(super) struct AfterSetup {
    pub pr: Option<String>,
    pub agent: Option<Box<QueuedLaunch>>,
    /// The agent that waits for a retried setup should setup fail.
    pub waiting: Option<WaitingAgent>,
}

/// Follow the preparation of the started workspace and settle a failed
/// setup, which leaves its agent waiting in the queue; a retry runs what
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
    let retry = Preparation {
        setup: true,
        post_setup: !reused,
        pr: rest.pr.clone(),
        agent: rest.agent.clone(),
    };
    let waiting = rest.waiting.as_mut();
    settle_setup(ctx, &workspace, prepared, Some(retry), waiting).await
}
