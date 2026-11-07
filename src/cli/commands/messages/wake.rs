//! Claude Code's wake hook: it waits in the background for the next update
//! of the idle agent and starts its next turn with it.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use super::Inbox;
use crate::{
    cli::{
        context::Context,
        installed::{arguments, replace_option, with_flag, without_option},
    },
    client::{links::Updates, space_names::SpaceSelector},
    installed::Executable,
    protocol::WakeEnd,
};

/// Claude Code's `asyncRewake` `Stop` hook: wait for the next messages or
/// item activity of the agent's workspace or chat, then exit 2, which starts
/// the idle agent's next turn with them. A failed wait is tried again, since
/// without one the agent stays idle; a newer delivery, the exit of `agent`,
/// the agent's process, or a signal ends it with 0 and leaves the agent
/// idle, and the daemon logs why. `space` and `resumed` continue the wait of
/// the hook an upgrade replaced.
pub(in crate::cli::commands) async fn wake(
    ctx: &Context,
    agent: u32,
    space: Option<SpaceSelector>,
    resumed: bool,
) -> Result<i32> {
    let mut wait = WakeWait {
        space: space.map(|space| space.to_string()),
        inbox: None,
        restarted: resumed,
    };
    let outcome = tokio::select! {
        next = retry_next_updates(ctx, &mut wait) => Ok(next),
        ended = agent_exit(agent) => Err(ended),
        ended = ending_signal() => Err(ended),
    };
    let (inbox, updates) = match outcome {
        // The daemon logs the wait a newer delivery took.
        Ok(None) => return Ok(0),
        Ok(Some(next)) => next,
        Err(ended) => {
            report_end(ctx, &wait, ended).await;
            return Ok(0);
        }
    };
    if !woken_turn_starts(ctx, &inbox).await {
        report_end(ctx, &wait, WakeEnd::Stopped).await;
        return Ok(0);
    }
    deliver(ctx, &inbox, &updates).await?;
    Ok(2)
}

/// Print `updates` for Claude Code, which starts the agent's turn with them
/// once the hook exits 2, and acknowledge them.
async fn deliver(ctx: &Context, inbox: &Inbox, updates: &Updates) -> Result<()> {
    eprintln!("{}", inbox.hook_context(updates));
    std::io::Write::flush(&mut std::io::stderr())?;
    if let Err(error) = inbox.acknowledge(ctx, updates).await {
        eprintln!("shoal: these updates may be delivered again: {error:#}");
    }
    Ok(())
}

/// Tell the daemon why the hook ends without starting the agent's turn, so
/// a missed wake can be traced, for the inbox the wait found or was named,
/// else the current one. Only Claude Code's debug output shows a failure.
async fn report_end(ctx: &Context, wait: &WakeWait, ended: WakeEnd) {
    let report = async {
        let space = match &wait.inbox {
            Some(inbox) => inbox.space.clone(),
            None => {
                Inbox::waited_for(ctx, wait.space.as_deref(), false)
                    .await?
                    .space
            }
        };
        let space = SpaceSelector::parse(&space);
        crate::client::agents::report_wake_end(&ctx.paths, space, ended).await
    };
    let limit = crate::agents::hooks::MESSAGE_DAEMON_LIMIT;
    let reported = tokio::time::timeout(limit, report)
        .await
        .context("the daemon did not answer in time");
    if let Err(error) = reported.and_then(|reported| reported) {
        eprintln!("shoal: the end of the wake hook was not reported: {error:#}");
    }
}

/// Resolves with the signal that ends the hook, which it catches to report
/// it: SIGTERM, SIGHUP or SIGINT. Never, when it can catch none.
async fn ending_signal() -> WakeEnd {
    use tokio::signal::unix::{SignalKind, signal};
    let kinds = [
        (SignalKind::terminate(), "SIGTERM"),
        (SignalKind::hangup(), "SIGHUP"),
        (SignalKind::interrupt(), "SIGINT"),
    ];
    let caught = kinds
        .into_iter()
        .filter_map(|(kind, name)| {
            let mut received = signal(kind).ok()?;
            Some(Box::pin(async move {
                received.recv().await;
                name
            }))
        })
        .collect::<Vec<_>>();
    if caught.is_empty() {
        return std::future::pending().await;
    }
    let (signal, _, _) = futures_util::future::select_all(caught).await;
    WakeEnd::Signalled {
        signal: signal.to_owned(),
    }
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

/// Resolves once process `agent` has exited, with how the hook saw it.
/// Claude Code runs hooks in their own process group, so stopping the
/// agent's group leaves the hook running, and a surviving process keeps
/// Shoal from proving the stop. A process that cannot be inspected counts as
/// exited, so the hook never outlives it.
async fn agent_exit(agent: u32) -> WakeEnd {
    use crate::process::identity;
    let agent = match identity::capture(agent) {
        Ok(Some(agent)) => agent,
        Ok(None) => return WakeEnd::AgentExited,
        Err(_) => return WakeEnd::AgentUnknown,
    };
    loop {
        match identity::alive(&agent) {
            Ok(true) => tokio::time::sleep(AGENT_POLL_INTERVAL).await,
            Ok(false) => return WakeEnd::AgentExited,
            Err(_) => return WakeEnd::AgentUnknown,
        }
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
async fn retry_next_updates(ctx: &Context, wait: &mut WakeWait) -> Option<(Inbox, Updates)> {
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
