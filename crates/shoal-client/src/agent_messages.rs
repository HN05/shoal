//! Messages for the agents working in a workspace, from the user or the daemon.
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, await_reply, request},
    files::NewFile,
    links::Updates,
    output::messages::Reached,
    protocol::AgentMethod,
    repository::Selector,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: i64,
    pub created_at: crate::time::UnixTime,
    /// Empty when the message is only its attachments.
    pub message: String,
    /// Shoal's copies of the files attached to the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<PathBuf>,
}

/// Queue `message` with `attachments` for the agents of `space`; the chat
/// `sender_chat` names, when its agent sends it, attaches its files.
pub async fn send(
    daemon: &impl Daemon,
    space: SpaceSelector,
    message: String,
    attachments: Vec<NewFile>,
    sender_chat: Option<String>,
) -> Result<()> {
    let method = AgentMethod::SendAgentMessage {
        space,
        message,
        attachments,
        sender_chat,
    };
    request(daemon, method).await
}

/// Queue `message` with `attachments` for every running agent, of
/// `repository` when it names one, returning the spaces it reached.
pub async fn send_to_all(
    daemon: &impl Daemon,
    repository: Option<Selector>,
    message: String,
    attachments: Vec<NewFile>,
    sender_chat: Option<String>,
) -> Result<Reached> {
    let method = AgentMethod::SendAllAgentMessage {
        repository,
        message,
        attachments,
        sender_chat,
    };
    request(daemon, method).await
}

/// The undelivered messages for the agents of `space`.
pub async fn undelivered(daemon: &impl Daemon, space: SpaceSelector) -> Result<Vec<AgentMessage>> {
    request(daemon, AgentMethod::AgentMessages { space }).await
}

/// Mark the messages `ids` of `space` delivered, so they do not come back.
pub async fn mark_delivered(
    daemon: &impl Daemon,
    space: SpaceSelector,
    ids: Vec<i64>,
) -> Result<()> {
    request(
        daemon,
        AgentMethod::MarkAgentMessagesDelivered { space, ids },
    )
    .await
}

/// The undelivered messages and queued item updates of `space`, without
/// waiting.
pub async fn updates(daemon: &impl Daemon, space: SpaceSelector) -> Result<Updates> {
    request(daemon, AgentMethod::AgentUpdates { space }).await
}

/// The next messages or item updates of `space`, waiting for them across
/// daemon restarts; `restarted` says the daemon restarted before the wait
/// began.
pub async fn await_updates(daemon: &impl Daemon, space: &str, restarted: bool) -> Result<Updates> {
    let method = || AgentMethod::WaitAgentUpdates {
        space: SpaceSelector::parse(space),
    };
    Updates::try_from(await_reply(daemon, method, restarted).await?)
}

/// Mark the item update `deliveries`, the `messages` and the leftover
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
