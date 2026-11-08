//! Waiting for item updates across daemon restarts.
use std::{ffi::OsString, time::Duration};

use anyhow::Result;
use tokio::time::Instant;

use crate::cli::{commands::messages, context::Context};
use crate::client::items;
use crate::client::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn watch_items(
    ctx: &Context,
    workspace: SpaceSelector,
    selection: crate::forge::items::link::Selection,
    timeout: Option<u64>,
    resumed: bool,
) -> Result<i32> {
    // The installed release continues the wait after an upgrade.
    let executable = crate::installed::Executable::current();
    let deadline = timeout.map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let updates = items::await_updates(&ctx.paths, &workspace, selection, deadline, resumed)
        .await
        .map_err(|error| {
            executable.resume_or(error, || resume_watch_arguments(&workspace, deadline))
        })?;
    ctx.show(&updates, |updates| {
        for message in &updates.messages {
            outln!(
                "Message: {}",
                messages::hanging_indent(&messages::shown(message), "         ")
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
    let deliveries = (updates.updates.iter())
        .map(|update| update.delivery.clone())
        .collect();
    let messages = updates.messages.iter().map(|message| message.id).collect();
    crate::client::agent_messages::acknowledge(
        &ctx.paths,
        workspace.clone(),
        deliveries,
        messages,
        Vec::new(),
    )
    .await?;
    Ok(0)
}

/// This watch's arguments continuing its wait in `space` until `deadline`, as
/// the installed release does after an upgrade restarted the daemon. A wait
/// an earlier release handed over names its workspace the earlier way, which
/// the space replaces.
fn resume_watch_arguments(space: &SpaceSelector, deadline: Option<Instant>) -> Vec<OsString> {
    use crate::cli::installed::{arguments, replace_option, with_flag, without_option};
    let resumed = without_option(arguments(), "--workspace");
    let resumed = without_option(resumed, "-s");
    let mut resumed = replace_option(resumed, "--space", &space.to_string());
    if let Some(seconds) = items::seconds_left(deadline) {
        resumed = replace_option(resumed, "--timeout", &seconds.to_string());
    }
    with_flag(resumed, "--resume")
}
