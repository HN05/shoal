//! Telling an agent that its turn's final message went unsent though the
//! command that marked the turn said it would go: the user's prompt
//! answered the turn, or sending failed when the turn ended.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_protocol::chat_marks::ChatMark;

use crate::{alerts::AlertKind, model::Space};

/// As text that follows "as".
fn destination(mark: &ChatMark) -> String {
    match mark {
        ChatMark::Post { thread } => format!("a post in thread {thread}"),
        ChatMark::Alert { to } => format!("an alert to {}", to.describe()),
    }
}

impl Spaces {
    /// Its next delivery shows the notice. A failure is logged, since the
    /// notice describes work that already ended.
    pub(super) async fn tell_unsent(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        mark: &ChatMark,
        why: &str,
    ) {
        let message = format!(
            "Your turn's final message was not sent as {}, which `shoal send --in-chat` marked: {why}. Send it again if it still applies.",
            destination(mark)
        );
        let kind = AlertKind::ChatMessageUnsent;
        let told = daemon.queue_agent_message_next_turn(&space.id, kind, message);
        if let Err(error) = told.await {
            crate::log!("unsent final message of {} not told: {error:#}", space.name);
        }
    }
}
