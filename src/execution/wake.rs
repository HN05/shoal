//! Waking an idle Codex, which has no hook that can start its turn: while it
//! is idle with undelivered updates, the wrapper types a fixed prompt into the
//! zmx session it runs in, and the prompt's `UserPromptSubmit` hook delivers
//! them. Claude Code wakes itself through its own hook.
use std::{ffi::OsString, time::Duration};

use anyhow::Result;

use crate::{agent::Agent, cli::client, paths::Paths, protocol::Method};

/// The prompt that starts the turn; the hook supplies the updates.
const PROMPT: &str = "Shoal: new messages or activity on this workspace's issues and PRs.";

/// Prompts the agent until dropped, when the agent's process ends.
pub(super) struct Waker(tokio::task::JoinHandle<()>);

impl Drop for Waker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The session to wake `agent` in: Codex on the terminal of a zmx session. A
/// detached launch has no terminal, though it may inherit a session's name.
pub(super) fn session(agent: Option<&str>, mode: &super::Mode) -> Option<OsString> {
    let codex = agent.and_then(|agent| agent.parse::<Agent>().ok()) == Some(Agent::Codex);
    (codex && !matches!(mode, super::Mode::Detached { .. }))
        .then(crate::cli::terminal::prompt_session)
        .flatten()
}

pub(super) fn start(paths: &Paths, workspace: &str, session: OsString) -> Waker {
    let paths = paths.clone();
    let workspace = workspace.to_owned();
    Waker(tokio::spawn(async move {
        loop {
            await_wake(&paths, &workspace, true).await;
            if let Err(error) = crate::cli::terminal::submit_prompt(&session, PROMPT).await {
                eprintln!("shoal: cannot prompt the agent with new updates: {error:#}");
            }
            // Prompt once per idle stretch: the agent takes the updates, or
            // stays idle until someone else starts its turn.
            await_wake(&paths, &workspace, false).await;
        }
    }))
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

/// A failed wait retries no sooner than this, so a persistent failure does
/// not spin.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);

async fn request(paths: &Paths, workspace: &str, due: bool) -> Result<()> {
    let method = Method::AwaitAgentWake {
        workspace: workspace.to_owned(),
        due,
    };
    let (_stream, body) = client::open(paths, method).await?;
    body.into_result().map(drop)
}
