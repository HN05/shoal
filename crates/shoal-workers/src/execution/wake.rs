//! Waking an idle agent that no hook of its own wakes, by typing a fixed
//! prompt into its terminal. The wrapper tells the daemon first, so the
//! keys do not count as the user's.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::time::Duration;

use anyhow::Result;

use crate::{
    agents::PromptWake,
    client::connection,
    client::connection::CommandPaths,
    model::ExecutionPlan,
    protocol::AgentMethod,
    terminal::{PromptSession, WAKE_PROMPT},
};
use shoal_protocol::space_names::SpaceSelector;

/// Prompts the agent until dropped, when the agent's process ends.
pub(super) struct Waker(tokio::task::JoinHandle<()>);

impl Drop for Waker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The session an agent's wrapper wakes it in, and when.
#[derive(Clone)]
pub(super) struct Wake {
    session: PromptSession,
    when: PromptWake,
}

/// How to wake an agent with `integration` that its wrapper wakes: on the
/// terminal of a session. A detached launch has no terminal, though it may
/// inherit a session's name.
pub(super) fn session(
    integration: Option<crate::ai::IntegrationKind>,
    mode: &super::Mode,
) -> Option<Wake> {
    let when = integration.and_then(crate::agents::prompt_wake)?;
    if matches!(mode, super::Mode::Detached { .. }) {
        return None;
    }
    Some(Wake {
        session: PromptSession::current()?,
        when,
    })
}

/// Wake the agent `plan` runs as `wake` says.
pub(super) fn start(paths: &CommandPaths, plan: &ExecutionPlan, wake: Wake) -> Waker {
    let paths = paths.clone();
    let (workspace, execution) = (plan.space.id.clone(), plan.id.clone());
    let hooked = (wake.when == PromptWake::WhenHookCannot).then(|| plan.id.clone());
    Waker(tokio::spawn(async move {
        // A host that needs the instance refuses prompts without it.
        let session = match wake.session.clone().pinned().await {
            Ok(session) => session,
            Err(error) => {
                eprintln!("shoal: cannot tell the agent's session apart: {error:#}");
                wake.session
            }
        };
        prompt_when_due(
            |due| await_wake(&paths, &workspace, due, hooked.clone()),
            || async {
                report_typing(&paths, &workspace, &execution).await;
                session.submit(WAKE_PROMPT).await
            },
        )
        .await
    }))
}

/// Prompt each time `await_due(true)` finds updates waiting for the idle
/// agent, then wait with `await_due(false)` until it took them. A failed
/// prompt reached nothing, so it is retried while prompting stays due.
async fn prompt_when_due<D, S>(mut await_due: impl FnMut(bool) -> D, mut submit: impl FnMut() -> S)
where
    D: std::future::Future<Output = ()>,
    S: std::future::Future<Output = Result<()>>,
{
    loop {
        await_due(true).await;
        if let Err(error) = submit().await {
            eprintln!("shoal: cannot prompt the agent with new updates: {error:#}");
            tokio::time::sleep(RETRY_INTERVAL).await;
            continue;
        }
        // Prompt once per idle stretch: the agent takes the updates, or
        // stays idle until someone else starts its turn.
        await_due(false).await;
    }
}

/// Wait until whether the agent is idle with updates matches `due`, waiting
/// out a restarting or failing daemon. When `hooked` is given, it counts
/// only the times the hook of that execution's agent cannot wake it.
async fn await_wake(
    paths: &CommandPaths,
    workspace: &SpaceId,
    due: bool,
    hooked: Option<ExecutionId>,
) {
    loop {
        match request(paths, workspace, due, hooked.clone()).await {
            Ok(()) => return,
            Err(_) => tokio::time::sleep(RETRY_INTERVAL).await,
        }
    }
}

/// Tell the daemon that the wrapper types into the terminal of `execution`
/// in `workspace` now. A failed report stays quiet, as the wrapper's other
/// reports do.
async fn report_typing(paths: &CommandPaths, workspace: &SpaceId, execution: &ExecutionId) {
    let report = AgentMethod::ReportTyping {
        workspace: SpaceSelector::id(workspace),
        execution: execution.clone(),
    };
    crate::proxy::send_report(paths, report).await;
}

/// A failed wait or prompt retries no sooner than this, so a persistent
/// failure does not spin.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);

async fn request(
    paths: &CommandPaths,
    workspace: &SpaceId,
    due: bool,
    hooked: Option<ExecutionId>,
) -> Result<()> {
    let method = AgentMethod::AwaitAgentWake {
        workspace: SpaceSelector::id(workspace),
        due,
        hooked,
    };
    let (_stream, body) = connection::open(paths, method).await?;
    body.into_result().map(drop)
}

#[cfg(test)]
mod tests;
