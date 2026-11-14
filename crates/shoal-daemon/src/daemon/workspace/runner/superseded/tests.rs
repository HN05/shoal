use super::super::tests::{
    Fixture, admit_and_claim, fixture, hold_the_builds_permit, job, job_entries,
};
use crate::{
    daemon::notifications::NotificationKind, forge::numbers::PullNumber, state::QueueState,
};

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
async fn a_queued_job_held_back_whose_pr_moved_on_leaves_the_queue_at_the_next_poll() {
    let fixture = fixture(
        vec![job("a", 4, 2), job("b", 5, 2)],
        "permit = \"builds\"\n[resources.builds]\n",
    )
    .await;
    let manager = &fixture.manager;
    hold_the_builds_permit(&fixture).await;
    admit_and_claim(manager).await;
    assert_eq!(job_entries(manager).await.len(), 2);

    move_pull_on(&fixture);
    let head = "GET /api/v1/repos/team/repo/pulls/12";
    let heads_read = received(&fixture, head);
    manager.refresh_waiting_jobs().await.unwrap();
    // One read of the PR's head and one cancel of the run serve both jobs.
    assert_eq!(received(&fixture, head) - heads_read, 1);
    let cancel = "POST /api/v1/repos/team/repo/actions/runs/2/cancel";
    assert_eq!(received(&fixture, cancel), 1);
    assert!(job_entries(manager).await.is_empty());
    manager.refresh_waiting_jobs().await.unwrap();
    assert!(job_entries(manager).await.is_empty());
    assert!(!cancel_failed(&fixture).await);
}

#[tokio::test]
async fn a_queued_job_whose_run_the_forge_does_not_cancel_is_passed_over_with_a_notification() {
    let fixture = fixture(vec![job("old", 4, 8)], "").await;
    let manager = &fixture.manager;
    move_pull_on(&fixture);

    manager.refresh_waiting_jobs().await.unwrap();
    assert!(job_entries(manager).await.is_empty());
    let notifications = manager.notifications(false, 50).await.unwrap();
    let run = format!("http://{}/team/repo/actions/runs/8", fixture.server.host);
    assert!(
        notifications.iter().any(|notification| {
            notification.kind == NotificationKind::RunnerFailed
                && notification.message.contains(&run)
        }),
        "{notifications:?}"
    );
    // The forge still lists it, but the runner leaves it to others.
    manager.refresh_waiting_jobs().await.unwrap();
    assert!(job_entries(manager).await.is_empty());
}

#[tokio::test]
async fn a_pr_whose_head_cannot_be_read_keeps_its_jobs_queued_and_the_rest_of_the_poll_goes_on() {
    let fixture = fixture(vec![job("unread", 4, 9), job("old", 5, 2)], "").await;
    let manager = &fixture.manager;
    move_pull_on(&fixture);

    manager.refresh_waiting_jobs().await.unwrap();
    let waiting = job_entries(manager).await;
    let handles: Vec<_> = (waiting.iter())
        .map(|entry| entry.job.as_ref().unwrap().job.handle.as_str())
        .collect();
    assert_eq!(handles, ["unread"]);
}

#[tokio::test]
async fn a_job_admitted_while_its_pr_is_checked_is_left_to_its_claim() {
    let fixture = fixture(vec![job("old", 4, 2)], "").await;
    let manager = &fixture.manager;
    manager.refresh_waiting_jobs().await.unwrap();
    let mut by_pull = manager
        .waiting_pull_jobs(&fixture.repository)
        .await
        .unwrap();
    let waiting = by_pull.remove(&PullNumber::new(12)).unwrap();

    manager.dispatch_queue().await.unwrap();
    manager
        .pass_over_superseded(&fixture.repository, &forge(&fixture).await, waiting)
        .await
        .unwrap();
    let entries = job_entries(manager).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].state, QueueState::Starting);
}

/// The forge `fixture`'s runner is registered with.
async fn forge(fixture: &Fixture) -> crate::forge::ForgeRepo {
    let registration = fixture
        .manager
        .runner_registration(&fixture.repository)
        .await
        .unwrap()
        .unwrap();
    crate::forge::ForgeRepo::resolve(
        &registration.forge_repository,
        &fixture.manager.forge_hosts(),
    )
    .await
    .unwrap()
}
