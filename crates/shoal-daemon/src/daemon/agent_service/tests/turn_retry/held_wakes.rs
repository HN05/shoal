use shoal_daemon_base::alerts::{AlertKind, NewAlert, Sender};
use shoal_protocol::links::Updates;

use super::*;

/// An agent whose rate-limited turn waits to be retried past `reset`,
/// with Shoal's alert queued meanwhile.
async fn limited_with_alert(reset: UnixTime) -> Running {
    let running = agent("").await;
    let (_, manager, workspace, execution, _) = &running;
    let failed = FailedTurn {
        failure: TurnFailure::RateLimited,
        resets_at: Some(reset),
    };
    fail(manager, workspace, execution.as_str(), failed).await;
    queue(manager, workspace, Sender::Shoal, "free some memory").await;
    running
}

/// A reset an hour off, past any wait of the test.
fn in_an_hour() -> UnixTime {
    now() + secs(60 * 60)
}

async fn queue(manager: &Manager, workspace: &Workspace, sender: Sender, text: &str) {
    let alert = NewAlert {
        sender,
        ..NewAlert::shoal(AlertKind::Message, Default::default(), text.into())
    };
    (manager.agents)
        .queue_alert(&workspace.id, alert, false)
        .await
        .unwrap();
}

/// The wake hook's wait for the agent of `workspace`, which a newer wait
/// or delivery would supersede.
fn wake_wait(
    manager: &std::sync::Arc<Manager>,
    workspace: &Workspace,
) -> tokio::task::JoinHandle<Updates> {
    let (manager, selector) = (manager.clone(), SpaceSelector::id(&workspace.id));
    tokio::spawn(async move {
        let wait = manager.links.wait_space_updates(&*manager, &selector);
        // A hang guard, never the behavior tested.
        tokio::time::timeout(secs(60), wait)
            .await
            .expect("the wake wait hung")
            .unwrap()
    })
}

fn texts(updates: &Updates) -> Vec<&str> {
    updates
        .alerts
        .iter()
        .map(|alert| alert.message.as_str())
        .collect()
}

async fn waiting(manager: &Manager, workspace: &Workspace) -> bool {
    let selector = SpaceSelector::id(&workspace.id);
    manager.updates_waiting(&selector).await.unwrap()
}

#[tokio::test]
async fn an_alert_waits_for_the_retry_of_a_rate_limited_turn() {
    let (_temp, manager, workspace, _execution, _connection) =
        limited_with_alert(in_an_hour()).await;
    assert!(!waiting(&manager, &workspace).await);
    let woken = wake_wait(&manager, &workspace);
    deliver_now(&manager).await;
    let woken = woken.await.unwrap();
    assert_eq!(
        texts(&woken),
        [
            "free some memory",
            &continuation(TurnFailure::RateLimited, 1)
        ]
    );
}

#[tokio::test]
async fn a_user_message_wakes_an_agent_whose_retry_waits() {
    let (_temp, manager, workspace, _execution, _connection) =
        limited_with_alert(in_an_hour()).await;
    let woken = wake_wait(&manager, &workspace);
    queue(&manager, &workspace, Sender::User, "status?").await;
    let woken = woken.await.unwrap();
    assert_eq!(texts(&woken), ["free some memory", "status?"]);
    assert!(waiting(&manager, &workspace).await);
}

#[tokio::test]
async fn a_retry_without_a_known_reset_holds_no_alert() {
    let (_temp, manager, workspace, execution, _connection) = agent("").await;
    fail(
        &manager,
        &workspace,
        execution.as_str(),
        TurnFailure::RateLimited,
    )
    .await;
    queue(&manager, &workspace, Sender::Shoal, "free some memory").await;
    assert!(waiting(&manager, &workspace).await);
}

#[tokio::test]
async fn a_reset_that_has_passed_holds_no_alert() {
    let reset = now() - RESET_MARGIN - secs(1);
    let (_temp, manager, workspace, _execution, _connection) = limited_with_alert(reset).await;
    assert!(waiting(&manager, &workspace).await);
}
