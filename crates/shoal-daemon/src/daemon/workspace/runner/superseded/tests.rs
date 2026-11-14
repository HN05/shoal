use super::super::tests::{admit_and_claim, fixture, job, job_entries};
use crate::daemon::notifications::NotificationKind;

#[tokio::test]
async fn a_job_whose_pr_moved_on_has_its_run_cancelled_without_a_workspace() {
    let fixture = fixture(vec![job("old", 4, 2)], Some("new"), "").await;
    let manager = &fixture.manager;

    admit_and_claim(manager).await;
    assert!(
        fixture
            .server
            .received()
            .iter()
            .any(|request| request.line == "POST /api/v1/repos/team/repo/actions/runs/2/cancel")
    );
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
    let notifications = manager.notifications(false, 50).await.unwrap();
    assert!(
        notifications
            .iter()
            .all(|notification| notification.kind != NotificationKind::RunnerFailed),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn a_superseded_run_the_forge_does_not_cancel_is_passed_over_with_a_notification() {
    let fixture = fixture(vec![job("old", 4, 8)], Some("new"), "").await;
    let manager = &fixture.manager;

    admit_and_claim(manager).await;
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
