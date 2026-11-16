//! Triggers: `[triggers.NAME]` config that starts agents when an event
//! source fires. Clients list them with their runtime state, and pause,
//! resume and run them.
use anyhow::Result;
use shoal_protocol::protocol::{ItemContext, TriggerMethod};
use shoal_protocol::triggers::{TriggerAction, TriggerRun, TriggerStatus};

use crate::connection::{Daemon, request};

/// The triggers of the repository `context` acts in and the global
/// config's, with their runtime state.
pub async fn list(
    daemon: &impl Daemon,
    context: Option<ItemContext>,
) -> Result<Vec<TriggerStatus>> {
    request(daemon, TriggerMethod::ListTriggers { context }).await
}

/// Pause the trigger `name`, or resume it when not `paused`.
pub async fn set_paused(
    daemon: &impl Daemon,
    context: Option<ItemContext>,
    name: String,
    paused: bool,
) -> Result<TriggerStatus> {
    let action = match paused {
        true => TriggerAction::Pause,
        false => TriggerAction::Resume,
    };
    request(
        daemon,
        TriggerMethod::ActOnTrigger {
            context,
            name,
            action,
        },
    )
    .await
}

/// Fire the trigger `name` now: the run it made, none when its filter
/// dropped the firing.
pub async fn run(
    daemon: &impl Daemon,
    context: Option<ItemContext>,
    name: String,
) -> Result<Option<TriggerRun>> {
    let action = TriggerAction::Run;
    request(
        daemon,
        TriggerMethod::ActOnTrigger {
            context,
            name,
            action,
        },
    )
    .await
}
