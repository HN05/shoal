//! What wakes the daemon's waits and background tasks when the state they
//! read changes. Each change is announced after it is stored, and each wait
//! subscribes before reading, so a change in between still wakes it.
use tokio::sync::{Notify, futures::Notified, watch};

#[derive(Default)]
pub struct Signals {
    messages: Counter,
    threads: Counter,
    item_updates: Counter,
    agents: Counter,
    cleanup: Notify,
    environment: Notify,
    turn_retry: Notify,
    wake_timers: Notify,
    execution_unknown: Notify,
    lost_reports: Notify,
}

impl Signals {
    /// Wake the waits for a space's messages.
    pub fn message_queued(&self) {
        self.messages.advance();
    }

    pub fn watch_messages(&self) -> watch::Receiver<u64> {
        self.messages.subscribe()
    }

    /// Wake waits for a post once a thread changes.
    pub fn thread_changed(&self) {
        self.threads.advance();
    }

    pub fn watch_threads(&self) -> watch::Receiver<u64> {
        self.threads.subscribe()
    }

    /// Wake watches once a background poll queued item updates.
    pub fn item_updates_queued(&self) {
        self.item_updates.advance();
    }

    pub fn watch_item_updates(&self) -> watch::Receiver<u64> {
        self.item_updates.subscribe()
    }

    /// Wake waits for an idle agent's updates once an agent's state changed
    /// or something was delivered to it.
    pub fn agent_changed(&self) {
        self.agents.advance();
    }

    pub fn watch_agents(&self) -> watch::Receiver<u64> {
        self.agents.subscribe()
    }

    /// Run the cleanup sweep now.
    pub fn wake_cleanup(&self) {
        self.cleanup.notify_one();
    }

    pub fn cleanup_woken(&self) -> Notified<'_> {
        self.cleanup.notified()
    }

    /// Check the environment again, after the config was reloaded.
    pub fn recheck_environment(&self) {
        self.environment.notify_one();
    }

    pub fn environment_recheck(&self) -> Notified<'_> {
        self.environment.notified()
    }

    /// Wake turn retry delivery, since a retry was scheduled.
    pub fn wake_turn_retry(&self) {
        self.turn_retry.notify_one();
    }

    pub fn turn_retry_woken(&self) -> Notified<'_> {
        self.turn_retry.notified()
    }

    /// Wake the wake timers' loop, since a timer was set.
    pub fn set_wake_timer(&self) {
        self.wake_timers.notify_one();
    }

    pub fn wake_timer_set(&self) -> Notified<'_> {
        self.wake_timers.notified()
    }

    /// Wake stopped-agent reconciliation, since an execution became unknown.
    pub fn execution_became_unknown(&self) {
        self.execution_unknown.notify_one();
    }

    pub fn execution_unknown(&self) -> Notified<'_> {
        self.execution_unknown.notified()
    }

    /// Look for the reports hooks left now, since a wrapper they may wait
    /// for reattached.
    pub fn wake_lost_reports(&self) {
        self.lost_reports.notify_one();
    }

    pub fn lost_reports_woken(&self) -> Notified<'_> {
        self.lost_reports.notified()
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
