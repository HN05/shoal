//! Triggers: `[triggers.NAME]` config that starts agents when an event
//! source fires. Clients list them with their runtime state, and pause,
//! resume and run them.
use anyhow::Result;
use shoal_protocol::protocol::{ItemContext, TriggerMethod};
use shoal_protocol::triggers::{Fired, TriggerStatus, WebhookRequest};

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
    request(
        daemon,
        TriggerMethod::PauseTrigger {
            context,
            name,
            paused,
        },
    )
    .await
}

/// Fire the trigger `name` now, a webhook trigger with `sample` as its
/// request: the run it made, or the filter condition that dropped it.
pub async fn run(
    daemon: &impl Daemon,
    context: Option<ItemContext>,
    name: String,
    sample: Option<WebhookRequest>,
) -> Result<Fired> {
    request(
        daemon,
        TriggerMethod::RunTrigger {
            context,
            name,
            request: sample,
        },
    )
    .await
}
