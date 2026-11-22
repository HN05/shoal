use tokio::sync::{mpsc, oneshot};

use super::*;
use crate::daemon::test_support::manager;

/// A platform whose announcements the test sends.
struct Fake(mpsc::UnboundedReceiver<PowerEvent<oneshot::Sender<()>>>);

impl PowerEvents for Fake {
    type Hold = oneshot::Sender<()>;

    async fn next(&mut self) -> Option<PowerEvent<oneshot::Sender<()>>> {
        self.0.recv().await
    }
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("sleep lock stalled")
}

#[tokio::test]
async fn a_sleep_holds_the_queue_until_the_machine_wakes_and_lets_the_sleep_go() {
    let (_root, manager) = manager().await;
    let (announce, announcements) = mpsc::unbounded_channel();
    let following = tokio::spawn({
        let manager = manager.clone();
        async move { follow(&manager, Fake(announcements)).await }
    });
    let (hold, released) = oneshot::channel();
    let allowance = Duration::from_secs(5);
    let sleeping = PowerEvent::Sleeping {
        hold: Some(hold),
        allowance,
    };
    announce.send(sleeping).unwrap();
    // The hold drops once the daemon is ready for the sleep.
    assert!(bounded(released).await.is_err());
    assert!(manager.agents.agent_queue().holds_admission());
    announce.send(PowerEvent::Woke).unwrap();
    drop(announce);
    bounded(following).await.unwrap();
    assert!(!manager.agents.agent_queue().holds_admission());
}
