use super::super::tests::{Fixture, fixture, job, job_entries};
use crate::daemon::notifications::NotificationKind;

/// Move PR 12 past the commit its runs wait at.
fn move_pull_on(fixture: &Fixture) {
    *fixture.pull_head.lock().unwrap() = "new".into();
}

/// How many requests `fixture`'s forge received for `line`.
fn received(fixture: &Fixture, line: &str) -> usize {
    (fixture.server.received().iter())
        .filter(|request| request.line == line)
        .count()
}

/// Whether the user heard that cancelling a superseded run failed.
async fn cancel_failed(fixture: &Fixture) -> bool {
    let notifications = fixture.manager.notifications(false, 50).await.unwrap();
    notifications
        .iter()
        .any(|notification| notification.kind == NotificationKind::RunnerFailed)
}

/// Admit the queued jobs and claim each.
async fn claim_admitted(manager: &crate::daemon::workspace::Manager) {
    manager.dispatch_queue().await.unwrap();
    for entry in manager.admitted_jobs().await.unwrap() {
        manager.claim_job(&entry).await;
    }
}

#[tokio::test]
async fn a_claimed_job_whose_pr_moved_on_has_its_run_cancelled_without_a_workspace() {
    let fixture = fixture(vec![job("old", 4, 2)], "").await;
    let manager = &fixture.manager;
    manager.refresh_waiting_jobs().await.unwrap();
    move_pull_on(&fixture);

    claim_admitted(manager).await;
    let cancel = "POST /api/v1/repos/team/repo/actions/runs/2/cancel";
    assert_eq!(received(&fixture, cancel), 1);
    assert!(job_entries(manager).await.is_empty());
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    assert!(
        manager
            .list_workspaces()
            .await
            .unwrap()
            .iter()
            .all(|workspace| !workspace.name.starts_with("ci-"))
    );
    // The forge still lists it, but the runner leaves it to others.
    manager.refresh_waiting_jobs().await.unwrap();
    assert!(job_entries(manager).await.is_empty());
    assert!(!cancel_failed(&fixture).await);
}

#[tokio::test]
async fn a_superseded_run_the_forge_does_not_cancel_is_passed_over_with_a_notification() {
    let fixture = fixture(vec![job("old", 4, 8)], "").await;
    let manager = &fixture.manager;
    manager.refresh_waiting_jobs().await.unwrap();
    move_pull_on(&fixture);

    claim_admitted(manager).await;
    assert!(job_entries(manager).await.is_empty());
    assert!(manager.claimed_jobs().await.unwrap().is_empty());
    let notifications = manager.notifications(false, 50).await.unwrap();
    let run = format!("http://{}/team/repo/actions/runs/8", fixture.server.host);
    assert!(
        notifications.iter().any(|notification| {
            notification.kind == NotificationKind::RunnerFailed
                && notification.message.contains(&run)
        }),
        "{notifications:?}"
    );
}
