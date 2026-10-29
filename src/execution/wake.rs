//! Waking an idle agent no hook of its own wakes: Codex, which has no hook
//! that can start its turn, and Claude Code after an interrupted turn, which
//! its wake hook does not follow. While it is idle so with undelivered
//! updates, the wrapper types a fixed prompt into the terminal session it
//! runs in, and the prompt's `UserPromptSubmit` hook delivers them.
use std::time::Duration;

use anyhow::Result;

use crate::{
    cli::{agents::PromptWake, client, terminal::PromptSession},
    model::ExecutionPlan,
    paths::Paths,
    protocol::AgentMethod,
};

/// The prompt that starts the turn; the hook supplies the updates.
const PROMPT: &str = "Shoal: new messages or activity on this workspace's issues and PRs.";

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
    let when = integration.and_then(crate::cli::agents::prompt_wake)?;
    if matches!(mode, super::Mode::Detached { .. }) {
        return None;
    }
    Some(Wake {
        session: PromptSession::current()?,
        when,
    })
}

/// Wake the agent `plan` runs as `wake` says.
pub(super) fn start(paths: &Paths, plan: &ExecutionPlan, wake: Wake) -> Waker {
    let paths = paths.clone();
    let workspace = plan.workspace.id.clone();
    let interrupted = (wake.when == PromptWake::AfterInterrupt).then(|| plan.id.clone());
    Waker(tokio::spawn(async move {
        prompt_when_due(
            |due| await_wake(&paths, &workspace, due, interrupted.clone()),
            || wake.session.submit(PROMPT),
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

/// Wait until whether the agent is idle with updates matches `due`, after an
/// interrupted turn of `interrupted` when given, waiting out a restarting or
/// failing daemon.
async fn await_wake(paths: &Paths, workspace: &str, due: bool, interrupted: Option<String>) {
    loop {
        match request(paths, workspace, due, interrupted.clone()).await {
            Ok(()) => return,
            Err(_) => tokio::time::sleep(RETRY_INTERVAL).await,
        }
    }
}

/// A failed wait or prompt retries no sooner than this, so a persistent
/// failure does not spin.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);

async fn request(
    paths: &Paths,
    workspace: &str,
    due: bool,
    interrupted: Option<String>,
) -> Result<()> {
    let method = AgentMethod::AwaitAgentWake {
        workspace: workspace.to_owned(),
        due,
        interrupted,
    };
    let (_stream, body) = client::open(paths, method).await?;
    body.into_result().map(drop)
}

#[cfg(test)]
mod tests;
