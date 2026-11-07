//! Claude Code's wake hook: it waits in the background for the next update
//! of the idle agent and starts its next turn with it.
use std::ffi::OsString;

use anyhow::Result;

use super::Inbox;
use crate::{
    cli::{
        context::Context,
        installed::{arguments, replace_option, with_flag, without_option},
    },
    client::links::Updates,
    installed::Executable,
};

/// Claude Code's `asyncRewake` `Stop` hook: wait for the next messages or
/// item activity of the agent's workspace or chat, then exit 2, which starts
/// the idle agent's next turn with them. A failed wait is tried again, since
/// without one the agent stays idle; a newer delivery or the exit of `agent`,
/// the agent's process, ends it with 0 and leaves the agent idle. `space`
/// and `resumed` continue the wait of the hook an upgrade replaced.
pub(in crate::cli::commands) async fn wake(
    ctx: &Context,
    agent: u32,
    space: Option<String>,
    resumed: bool,
) -> Result<i32> {
    let wait = WakeWait {
        space,
        inbox: None,
        restarted: resumed,
    };
    let next = tokio::select! {
        next = retry_next_updates(ctx, wait) => next,
        () = agent_exit(agent) => return Ok(0),
    };
    let Some((inbox, updates)) = next else {
        return Ok(0);
    };
    if !woken_turn_starts(ctx, &inbox).await {
        return Ok(0);
    }
    eprintln!("{}", inbox.hook_context(&updates));
    std::io::Write::flush(&mut std::io::stderr())?;
    if let Err(error) = inbox.acknowledge(ctx, &updates).await {
        eprintln!("shoal: these updates may be delivered again: {error:#}");
    }
    Ok(2)
}

/// Report the turn the wake starts before delivering its updates: Claude
/// Code fires no hook when a hook's exit starts a turn, so the agent would
/// show idle until its first tool call, and an idle stop could end the turn
/// that consumed the update meant to restore it. Whether to deliver them:
/// not when a stop of the agent came first, which leaves them unread for its
/// restore. A failed report still delivers, since without the turn the agent
/// would stay idle.
async fn woken_turn_starts(ctx: &Context, inbox: &Inbox) -> bool {
    super::super::agent_state::start_woken_turn(ctx, inbox.space.clone())
        .await
        .unwrap_or_else(|error| {
            eprintln!("shoal: the woken turn was not reported: {error:#}");
            true
        })
}

/// How often the wake hook checks that its agent still runs.
const AGENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Resolves once process `agent` has exited. Claude Code runs hooks in their
/// own process group, so stopping the agent's group leaves the hook running,
/// and a surviving process keeps Shoal from proving the stop. A process that
/// cannot be identified counts as exited, so the hook never outlives it.
async fn agent_exit(agent: u32) {
    use crate::process::identity;
    let Ok(Some(agent)) = identity::capture(agent) else {
        return;
    };
    while identity::alive(&agent).unwrap_or(false) {
        tokio::time::sleep(AGENT_POLL_INTERVAL).await;
    }
}

/// The first retry of a failed wake wait; the delay doubles up to the cap.
const WAKE_RETRY_INITIAL: std::time::Duration = std::time::Duration::from_secs(1);
/// A lasting failure is tried again this often.
const WAKE_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// [`WakeWait::next`], tried again after each failure, such as a daemon that
/// has not come back or does not yet know the agent's scope. After an upgrade
/// changed the daemon protocol, the installed release waits instead. Only
/// the first failure is reported: whatever the hook prints reaches the agent
/// with the updates that wake it, and a lasting failure would repeat there
/// once per retry.
async fn retry_next_updates(ctx: &Context, mut wait: WakeWait) -> Option<(Inbox, Updates)> {
    let executable = Executable::current();
    let mut delay = WAKE_RETRY_INITIAL;
    let mut reported = false;
    loop {
        match wait.next(ctx).await {
            Ok(next) => return next,
            Err(error) => {
                let error = executable.resume_or(error, || wait.resume_arguments(arguments()));
                if !std::mem::replace(&mut reported, true) {
                    eprintln!(
                        "shoal: waiting for agent updates failed; trying again until it succeeds: {error:#}"
                    );
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(WAKE_RETRY_MAX);
            }
        }
    }
}

/// The wake hook's wait: the space it was named, its inbox once found, and
/// whether the daemon restarted before the next wait began, so it does not
/// know the agent's scope until the agent's wrapper reattaches.
struct WakeWait {
    space: Option<String>,
    inbox: Option<Inbox>,
    restarted: bool,
}

impl WakeWait {
    /// The inbox's next messages or item activity, or `None` when a newer
    /// delivery takes them.
    async fn next(&mut self, ctx: &Context) -> Result<Option<(Inbox, Updates)>> {
        let inbox = match &self.inbox {
            Some(inbox) => inbox,
            None => {
                let inbox = Inbox::waited_for(ctx, self.space.as_deref(), self.restarted).await?;
                self.inbox.insert(inbox)
            }
        };
        let updates = inbox.next(ctx, std::mem::take(&mut self.restarted)).await?;
        if updates.updates.is_empty() && updates.messages.is_empty() {
            return Ok(None);
        }
        Ok(Some((inbox.clone(), updates)))
    }

    /// The hook's `arguments` continuing this wait after an upgrade restarted
    /// the daemon, in the inbox it found or was named. A hook an earlier
    /// release handed over names its workspace the earlier way, which the
    /// space replaces.
    fn resume_arguments(&self, arguments: Vec<OsString>) -> Vec<OsString> {
        let space = self.inbox.as_ref().map(|inbox| inbox.space.as_str());
        let arguments = match space.or(self.space.as_deref()) {
            Some(space) => {
                let arguments = without_option(arguments, "--workspace");
                replace_option(arguments, "--space", space)
            }
            None => arguments,
        };
        with_flag(arguments, "--resume")
    }
}

#[cfg(test)]
mod tests;
