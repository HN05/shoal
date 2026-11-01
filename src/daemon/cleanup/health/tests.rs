use super::*;

#[test]
fn health_reports_failing_and_stalled_cleanup() {
    let mut health = Health::default();
    let (start, unix) = (health.started.at, health.started.unix);
    let after = |seconds| start + Duration::from_secs(seconds);
    // Wall time runs on while the Mac sleeps; the monotonic clock does not.
    let moment = |seconds, slept| Moment {
        at: after(seconds),
        unix: unix + seconds + slept,
    };
    assert_eq!(health.problem(start), None);
    let stalled = start + STALLED_AFTER;
    assert!(
        health
            .problem(stalled)
            .unwrap()
            .contains("since the daemon started")
    );

    assert_eq!(health.record(moment(30, 0), &Ok(())), None);
    assert_eq!(health.record(moment(60, 3600), &Ok(())), None);
    assert_eq!(health.problem(after(90)), None);

    let failed = Err(anyhow::anyhow!("completion cleanup: disk is full"));
    assert_eq!(
        health.record(moment(90, 3600), &failed).as_deref(),
        Some("completion cleanup: disk is full")
    );
    assert_eq!(
        health.record(moment(120, 3600), &failed).as_deref(),
        Some("completion cleanup: disk is full"),
        "an unrecorded notification is retried"
    );
    health.failure.as_mut().unwrap().notified = true;
    assert_eq!(health.record(moment(150, 3600), &failed), None);
    let problem = health.problem(after(150)).unwrap();
    assert!(problem.contains("failed 3 consecutive passes"), "{problem}");
    assert!(problem.ends_with("completion cleanup: disk is full"));

    assert_eq!(health.record(moment(180, 3600), &Ok(())), None);
    assert_eq!(health.problem(after(180)), None);
    assert!(health.problem(after(180) + STALLED_AFTER).is_some());
}

#[tokio::test]
async fn failing_passes_notify_once_and_show_in_status() {
    use crate::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "done".into(), None, None, None)
        .await
        .unwrap();
    for _ in 0..2 {
        manager
            .record_sweep(Err(anyhow::anyhow!("completion cleanup: disk is full")))
            .await;
    }
    let notifications = manager.notifications(true, 100).await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].workspace, None);
    assert_eq!(notifications[0].kind, NotificationKind::CleanupFailed);
    assert_eq!(
        notifications[0].message,
        "automatic cleanup failed: completion cleanup: disk is full"
    );
    let status = manager.workspace_status(&workspace.id).await.unwrap();
    assert!(
        status
            .cleanup_error
            .unwrap()
            .contains("failed 2 consecutive passes since")
    );

    manager.record_sweep(Ok(())).await;
    let status = manager.workspace_status(&workspace.id).await.unwrap();
    assert_eq!(status.cleanup_error, None);
}
