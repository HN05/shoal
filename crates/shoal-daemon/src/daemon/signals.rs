//! What wakes the daemon's waits and background tasks when the state they
//! read changes. Each change is announced after it is stored, and each wait
//! subscribes before reading, so a change in between still wakes it.
use tokio::sync::{Notify, futures::Notified, watch};

#[derive(Default)]
pub(crate) struct Signals {
    messages: Counter,
    questions: Counter,
    item_updates: Counter,
    agents: Counter,
    cleanup: Notify,
    environment: Notify,
    turn_retry: Notify,
    wake_timers: Notify,
    execution_unknown: Notify,
    lost_reports: Notify,
    usage: Notify,
}

impl Signals {
    /// Wake the waits for a space's messages.
    pub(crate) fn message_queued(&self) {
        self.messages.advance();
    }

    pub(crate) fn watch_messages(&self) -> watch::Receiver<u64> {
        self.messages.subscribe()
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

    /// Wake the wake timers' loop, since a timer was set.
    pub(crate) fn set_wake_timer(&self) {
        self.wake_timers.notify_one();
    }

    pub(crate) fn wake_timer_set(&self) -> Notified<'_> {
        self.wake_timers.notified()
    }

    /// Wake stopped-agent reconciliation, since an execution became unknown.
    pub(crate) fn execution_became_unknown(&self) {
        self.execution_unknown.notify_one();
    }

    pub(crate) fn execution_unknown(&self) -> Notified<'_> {
        self.execution_unknown.notified()
    }

    /// Look for the reports hooks left now, since a wrapper they may wait
    /// for reattached.
    pub(crate) fn wake_lost_reports(&self) {
        self.lost_reports.notify_one();
    }

    pub(crate) fn lost_reports_woken(&self) -> Notified<'_> {
        self.lost_reports.notified()
    }

    /// Sample the workspaces' usage now, since a workspace's containers
    /// started.
    pub(crate) fn containers_started(&self) {
        self.usage.notify_one();
    }

    pub(crate) fn usage_woken(&self) -> Notified<'_> {
        self.usage.notified()
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
