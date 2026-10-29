use std::time::Duration;

use anyhow::bail;
use tokio::sync::{mpsc, watch};

use super::*;
use crate::test_support::manager;

/// A login manager whose announcements the test sends, counting held locks.
struct Fake {
    announcements: mpsc::UnboundedReceiver<bool>,
    locks: Arc<watch::Sender<usize>>,
    refuse: bool,
}

struct Held(Arc<watch::Sender<usize>>);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.send_modify(|held| *held -= 1);
    }
}

impl LoginManager for Fake {
    type Lock = Held;

    async fn delay_shutdown(&self) -> Result<Held> {
        if self.refuse {
            bail!("interactive authentication required");
        }
        self.locks.send_modify(|held| *held += 1);
        Ok(Held(self.locks.clone()))
    }

    async fn announcement(&mut self) -> Option<bool> {
        self.announcements.recv().await
    }
}

struct Harness {
    announce: mpsc::UnboundedSender<bool>,
    locks: watch::Receiver<usize>,
    holding: tokio::task::JoinHandle<()>,
}

fn start(manager: &Arc<Manager>, refuse: bool) -> Harness {
    let (announce, announcements) = mpsc::unbounded_channel();
    let locks = Arc::new(watch::Sender::new(0));
    let fake = Fake {
        announcements,
        locks: locks.clone(),
        refuse,
    };
    let holding = tokio::spawn({
        let manager = manager.clone();
        async move { hold(&manager, fake).await }
    });
    Harness {
        announce,
        locks: locks.subscribe(),
        holding,
    }
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("shutdown lock stalled")
}

impl Harness {
    async fn held(&mut self, count: usize) {
        bounded(self.locks.wait_for(|held| *held == count))
            .await
            .unwrap();
    }

    async fn end(self) {
        drop(self.announce);
        bounded(self.holding).await.unwrap();
    }
}

#[tokio::test]
async fn a_shutdown_releases_the_lock_after_stopping_and_a_cancellation_takes_it_again() {
    let (_root, manager) = manager().await;
    let mut harness = start(&manager, false);
    harness.held(1).await;
    assert!(!manager.agent_queue.suspended());
    // A cancellation before any shutdown changes nothing.
    harness.announce.send(false).unwrap();
    harness.announce.send(true).unwrap();
    harness.held(0).await;
    assert!(manager.agent_queue.suspended());
    harness.announce.send(false).unwrap();
    harness.held(1).await;
    assert!(!manager.agent_queue.suspended());
    harness.end().await;
}

#[tokio::test]
async fn a_refused_lock_is_reported_and_shutdowns_still_stop_work() {
    let (_root, manager) = manager().await;
    let harness = start(&manager, true);
    harness.announce.send(true).unwrap();
    harness.end().await;
    assert!(manager.agent_queue.suspended());
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(
        notifications.iter().any(|n| {
            n.kind == NotificationKind::EnvironmentProblem
                && n.message
                    == "Shoal could not delay system shutdowns to stop agents first: interactive authentication required"
        }),
        "{notifications:?}"
    );
}
