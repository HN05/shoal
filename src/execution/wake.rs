//! Waking an idle Codex, which has no hook that can start its turn: while it
//! is idle with undelivered updates, the wrapper types a fixed prompt into the
//! terminal session it runs in, and the prompt's `UserPromptSubmit` hook
//! delivers them. Claude Code wakes itself through its own hook.
use std::time::Duration;

use anyhow::Result;

use crate::{
    cli::{client, terminal::PromptSession},
    paths::Paths,
    protocol::Method,
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

/// The session to wake `agent` in: Codex on the terminal of a session. A
/// detached launch has no terminal, though it may inherit a session's name.
pub(super) fn session(agent: Option<&str>, mode: &super::Mode) -> Option<PromptSession> {
    let prompted = agent.is_some_and(crate::cli::agents::wakes_by_prompt);
    (prompted && !matches!(mode, super::Mode::Detached { .. }))
        .then(PromptSession::current)
        .flatten()
}

pub(super) fn start(paths: &Paths, workspace: &str, session: PromptSession) -> Waker {
    let paths = paths.clone();
    let workspace = workspace.to_owned();
    Waker(tokio::spawn(async move {
        prompt_when_due(
            |due| await_wake(&paths, &workspace, due),
            || session.submit(PROMPT),
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
/// out a restarting or failing daemon.
async fn await_wake(paths: &Paths, workspace: &str, due: bool) {
    loop {
        match request(paths, workspace, due).await {
            Ok(()) => return,
            Err(_) => tokio::time::sleep(RETRY_INTERVAL).await,
        }
    }
}

/// A failed wait or prompt retries no sooner than this, so a persistent
/// failure does not spin.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);

async fn request(paths: &Paths, workspace: &str, due: bool) -> Result<()> {
    let method = Method::AwaitAgentWake {
        workspace: workspace.to_owned(),
        due,
    };
    let (_stream, body) = client::open(paths, method).await?;
    body.into_result().map(drop)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_failed_prompt_is_retried_while_it_stays_due() {
        let sent = AtomicUsize::new(0);
        let prompting = prompt_when_due(
            // Prompting stays due; once taken, the agent never idles again.
            |due| async move {
                if !due {
                    std::future::pending::<()>().await;
                }
            },
            || async {
                match sent.fetch_add(1, Ordering::SeqCst) {
                    0 => anyhow::bail!("zmx is not running"),
                    _ => Ok(()),
                }
            },
        );
        // A hang guard only: the paused clock skips the retry interval.
        let _ = tokio::time::timeout(RETRY_INTERVAL * 10, prompting).await;
        assert_eq!(sent.load(Ordering::SeqCst), 2);
    }
}
