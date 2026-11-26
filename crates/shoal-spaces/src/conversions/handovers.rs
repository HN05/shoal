//! The hand-overs of converted spaces' agents, one at a time. A task that
//! holds them may take them again within, as a relocation's removal stops
//! the agents of the spaces below it. Whatever takes both these and a
//! repository's gates, such as a conversion, a repository's removal or a
//! repair, takes these first.
use std::future::Future;

tokio::task_local! {
    /// Set within a hand-over this task holds.
    static HELD: ();
}

#[derive(Default)]
pub struct Handovers(tokio::sync::Mutex<()>);

impl Handovers {
    /// A task that holds them already goes on holding them.
    pub async fn hold<F: Future>(&self, work: F) -> F::Output {
        if HELD.try_with(|()| ()).is_ok() {
            return work.await;
        }
        let _held = match self.0.try_lock() {
            Ok(held) => held,
            Err(_) => {
                #[cfg(any(test, feature = "test-support"))]
                WAITING.notify_one();
                self.0.lock().await
            }
        };
        HELD.scope((), work).await
    }
}

/// Notified when a hold waits for another, which tests synchronize on.
#[cfg(any(test, feature = "test-support"))]
pub static WAITING: tokio::sync::Notify = tokio::sync::Notify::const_new();
