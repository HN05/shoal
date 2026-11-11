//! What wakes the daemon's waits and background tasks when the state they
//! read changes. Each change is announced after it is stored, and each wait
//! subscribes before reading, so a change in between still wakes it.
use tokio::sync::{Notify, futures::Notified, watch};

#[derive(Default)]
pub(crate) struct Signals {
    /// The newest recorded notification ID.
    notifications: watch::Sender<i64>,
    agent_messages: Counter,
    chat_messages: Counter,
    questions: Counter,
    item_updates: Counter,
    agents: Counter,
    cleanup: Notify,
    environment: Notify,
    turn_retry: Notify,
    execution_unknown: Notify,
}

impl Signals {
    /// Wake `shoal alerts --follow` for the notification `id`.
    pub(crate) fn notification_recorded(&self, id: i64) {
        self.notifications.send_replace(id);
    }

    pub(crate) fn watch_notifications(&self) -> watch::Receiver<i64> {
        self.notifications.subscribe()
    }

    /// Wake watches waiting for agent messages.
    pub(crate) fn agent_message_queued(&self) {
        self.agent_messages.advance();
    }

    pub(crate) fn watch_agent_messages(&self) -> watch::Receiver<u64> {
        self.agent_messages.subscribe()
    }

    /// Wake chats waiting for messages.
    pub(crate) fn chat_message_queued(&self) {
        self.chat_messages.advance();
    }

    pub(crate) fn watch_chat_messages(&self) -> watch::Receiver<u64> {
        self.chat_messages.subscribe()
    }

    /// Wake waits for an answer once a question is answered or closed.
    pub(crate) fn question_changed(&self) {
        self.questions.advance();
    }

    pub(crate) fn watch_questions(&self) -> watch::Receiver<u64> {
        self.questions.subscribe()
    }

    /// Wake watches once a background poll queued item updates.
    pub(crate) fn item_updates_queued(&self) {
        self.item_updates.advance();
    }

    pub(crate) fn watch_item_updates(&self) -> watch::Receiver<u64> {
        self.item_updates.subscribe()
    }

    /// Wake waits for an idle agent's updates once an agent's state changed
    /// or something was delivered to it.
    pub(crate) fn agent_changed(&self) {
        self.agents.advance();
    }

    pub(crate) fn watch_agents(&self) -> watch::Receiver<u64> {
        self.agents.subscribe()
    }

    /// Run the cleanup sweep now.
    pub(crate) fn wake_cleanup(&self) {
        self.cleanup.notify_one();
    }

    pub(crate) fn cleanup_woken(&self) -> Notified<'_> {
        self.cleanup.notified()
    }

    /// Check the environment again, after the config was reloaded.
    pub(crate) fn recheck_environment(&self) {
        self.environment.notify_one();
    }

    pub(crate) fn environment_recheck(&self) -> Notified<'_> {
        self.environment.notified()
    }

    /// Wake turn retry delivery, since a retry was scheduled.
    pub(crate) fn wake_turn_retry(&self) {
        self.turn_retry.notify_one();
    }

    pub(crate) fn turn_retry_woken(&self) -> Notified<'_> {
        self.turn_retry.notified()
    }

    /// Wake stopped-agent reconciliation, since an execution became unknown.
    pub(crate) fn execution_became_unknown(&self) {
        self.execution_unknown.notify_one();
    }

    pub(crate) fn execution_unknown(&self) -> Notified<'_> {
        self.execution_unknown.notified()
    }
}

/// A count that wakes its subscribers each time it advances.
#[derive(Default)]
struct Counter(watch::Sender<u64>);

impl Counter {
    fn advance(&self) {
        self.0.send_modify(|count| *count += 1);
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.subscribe()
    }
}
