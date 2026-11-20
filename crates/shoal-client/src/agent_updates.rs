//! The alerts and item updates a space's agent receives.
use anyhow::Result;

use crate::connection::{Daemon, await_reply, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    links::Updates,
    protocol::{AgentMethod, WaitingUpdates},
};

/// The unread alerts and queued item updates of `space`, without
/// waiting.
pub async fn updates(daemon: &impl Daemon, space: SpaceSelector) -> Result<Updates> {
    request(daemon, AgentMethod::AgentUpdates { space }).await
}

/// Whether messages that wake an idle agent or item updates a hook would
/// deliver wait for the agent of `space`, which stay unread.
pub async fn waiting(daemon: &impl Daemon, space: SpaceSelector) -> Result<bool> {
    let waiting: WaitingUpdates = request(daemon, AgentMethod::UpdatesWaiting { space }).await?;
    Ok(waiting.waiting)
}

/// The next alerts or item updates of `space`, waiting for them across
/// daemon restarts; `restarted` says the daemon restarted before the wait
/// began.
pub async fn await_updates(
    daemon: &impl Daemon,
    space: &SpaceSelector,
    restarted: bool,
) -> Result<Updates> {
    let method = || AgentMethod::WaitAgentUpdates {
        space: space.clone(),
    };
    Updates::try_from(await_reply(daemon, method, restarted).await?)
}

/// Mark the item update `deliveries`, the alerts `messages` and the leftover
/// processes `leftovers` of `space` shown, so they do not come back.
pub async fn acknowledge(
    daemon: &impl Daemon,
    space: SpaceSelector,
    deliveries: Vec<String>,
    messages: Vec<i64>,
    leftovers: Vec<u32>,
) -> Result<()> {
    let method = AgentMethod::AcknowledgePrUpdates {
        space,
        deliveries,
        messages,
        leftovers,
    };
    request(daemon, method).await
}
