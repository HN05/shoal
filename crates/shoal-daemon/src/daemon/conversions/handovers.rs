//! The hand-overs of converted spaces' agents: one at a time, held by the
//! relocation, the stops of a converting workspace's agents and the making
//! of a chat's workspace, and held again by nothing within one of them, as
//! a relocation's removal stops the agents of the spaces below it. They come
//! before a repository's gates: whatever takes both, such as a conversion,
//! a repository's removal or a repair, takes them first.
use std::future::Future;

tokio::task_local! {
    /// Set within a hand-over this task holds.
    static HELD: ();
}

/// What a hand-over holds.
#[derive(Default)]
pub(crate) struct Handovers(tokio::sync::Mutex<()>);

impl Handovers {
    /// Run `work` holding the hand-overs, unless this task holds them
    /// already, which then goes on holding them.
    pub(crate) async fn hold<F: Future>(&self, work: F) -> F::Output {
        if HELD.try_with(|()| ()).is_ok() {
            return work.await;
        }
        let _held = match self.0.try_lock() {
            Ok(held) => held,
            Err(_) => {
                #[cfg(test)]
                WAITING.notify_one();
                self.0.lock().await
            }
        };
        HELD.scope((), work).await
    }
}

/// Notified when a hold waits for another, which tests synchronize on.
#[cfg(test)]
pub(crate) static WAITING: tokio::sync::Notify = tokio::sync::Notify::const_new();
