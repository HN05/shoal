//! Telling an agent that its turn's final message went unsent, though the
//! command that marked the turn said it would go: the user's prompt
//! answered the turn, or sending it failed when the turn ended.
use shoal_protocol::chat_marks::ChatMark;

use crate::{
    daemon::{alerts::AlertKind, manager::Manager, messages::Delivery},
    model::Space,
};

/// Where `mark` sends the final message, as text that follows "as".
fn destination(mark: &ChatMark) -> String {
    match mark {
        ChatMark::Post { thread } => format!("a post in thread {thread}"),
        ChatMark::Alert { to } => format!("an alert to {}", to.describe()),
    }
}

impl Manager {
    /// Tell the agent of `space` that its turn's final message did not go
    /// as `mark` said, `why`; its next delivery shows it. A failure is
    /// logged, since this notice describes work that already ended.
    pub(super) async fn tell_unsent(&self, space: &Space, mark: &ChatMark, why: &str) {
        let message = format!(
            "Your turn's final message was not sent as {}, which `shoal send --in-chat` marked: {why}. Send it again if it still applies.",
            destination(mark)
        );
        let kind = AlertKind::ChatMessageUnsent;
        let told =
            self.agents
                .queue_agent_message_delivered(&space.id, kind, message, Delivery::NextTurn);
        if let Err(error) = told.await {
            crate::daemon::log!("unsent final message of {} not told: {error:#}", space.name);
        }
    }
}
