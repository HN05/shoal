//! Waiting for item updates across daemon restarts.
use std::{ffi::OsString, time::Duration};

use anyhow::Result;
use tokio::time::Instant;

use crate::{
    cli::{
        client::{self, request},
        commands::messages,
        context::Context,
    },
    forge::items::link::Selection,
    protocol::{AgentMethod, Body, ItemMethod},
};

pub(in crate::cli::commands) async fn watch_items(
    ctx: &Context,
    workspace: String,
    selection: crate::forge::items::link::Selection,
    timeout: Option<u64>,
    resumed: bool,
) -> Result<i32> {
    // The installed release continues the wait after an upgrade.
    let executable = crate::cli::installed::Executable::current();
    let deadline = timeout.map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let body = wait_for_items(ctx, &workspace, selection, deadline, resumed)
        .await
        .map_err(|error| {
            executable.resume_or(error, || resume_watch_arguments(&workspace, deadline))
        })?;
    let updates = crate::daemon::links::wait::Updates::try_from(body)?;
    ctx.show(&updates, |updates| {
        for message in &updates.messages {
            outln!(
                "Message: {}",
                messages::hanging_indent(&message.message, "         ")
            );
        }
        for update in &updates.updates {
            outln!("{update}");
        }
        if updates.timed_out {
            outln!("No item updates before timeout.");
        }
        if updates.superseded {
            outln!("A newer wait in this workspace receives the updates.");
        }
    })?;
    crate::stdout::flush()?;
    if updates.updates.is_empty() && updates.messages.is_empty() {
        return Ok(0);
    }
    request::<()>(
        &ctx.paths,
        AgentMethod::AcknowledgePrUpdates {
            workspace,
            deliveries: updates
                .updates
                .iter()
                .map(|update| update.delivery.clone())
                .collect(),
            messages: updates.messages.iter().map(|message| message.id).collect(),
            attached: None,
        },
    )
    .await?;
    Ok(0)
}

/// A daemon restart closes the wait, so wait again on the next daemon until
/// `deadline`; `restarted` says the daemon restarted before this wait began.
/// Waiting acknowledges nothing, and undelivered updates stay pending for the
/// next wait.
pub(in crate::cli::commands) async fn wait_for_items(
    ctx: &Context,
    workspace: &str,
    selection: Selection,
    deadline: Option<Instant>,
    restarted: bool,
) -> Result<Body> {
    let method = || ItemMethod::WatchItems {
        workspace: workspace.to_owned(),
        selection: selection.clone(),
        timeout_secs: seconds_left(deadline),
    };
    let reply = client::await_reply(&ctx.paths, method, restarted);
    let Some(deadline) = deadline else {
        return reply.await;
    };
    match tokio::time::timeout_at(deadline, reply).await {
        Ok(reply) => reply,
        Err(_) => Ok(Body::PrUpdates(crate::daemon::links::wait::Updates {
            updates: Vec::new(),
            messages: Vec::new(),
            timed_out: true,
            superseded: false,
            attached: None,
        })),
    }
}

/// The whole seconds the daemon waits before `deadline`, at least one.
fn seconds_left(deadline: Option<Instant>) -> Option<u64> {
    let left = deadline?.saturating_duration_since(Instant::now());
    Some((left.as_secs() + u64::from(left.subsec_nanos() > 0)).max(1))
}

/// This watch's arguments continuing its wait in `workspace` until `deadline`,
/// as the installed release does after an upgrade restarted the daemon.
fn resume_watch_arguments(workspace: &str, deadline: Option<Instant>) -> Vec<OsString> {
    use crate::cli::installed::{arguments, replace_option, with_flag};
    let mut resumed = replace_option(arguments(), "--workspace", workspace);
    if let Some(seconds) = seconds_left(deadline) {
        resumed = replace_option(resumed, "--timeout", &seconds.to_string());
    }
    with_flag(resumed, "--resume")
}

#[cfg(test)]
mod tests;
