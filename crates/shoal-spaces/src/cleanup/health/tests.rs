use crate::cleanup::health::*;
use crate::notifications::AlertKind;
use crate::stand_in::Fixture;
use shoal_daemon_base::failures::SweepFailures;
use std::time::Duration;

use crate::cleanup::SpaceFailures;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// The failure is in the workspace named after `workspace` when one is
/// given.
fn failed(step: &str, workspace: Option<&str>, error: &str) -> Result<(), SweepFailures> {
    let error = anyhow::anyhow!("{error}");
    let result = match workspace {
        Some(id) => {
            let mut failures = SpaceFailures::default();
            failures.push(&SpaceId::from(id), &id.into(), &error);
            failures.into_result()
        }
        None => Err(error),
    };
    let mut failed = SweepFailures::default();
    failed.step(step, result);
    failed.into_result()
}

fn unreported(workspace: Option<&str>, error: &str) -> Unreported {
    Unreported {
        workspace: workspace.map(|id| (SpaceId::from(id), id.into())),
        error: error.to_owned(),
    }
}

#[test]
fn health_reports_failing_and_stalled_cleanup() {
    let mut health = Health::default();
    let (start, unix) = (health.started.at, health.started.unix);
    let after = |seconds| start + Duration::from_secs(seconds);
    // Wall time runs on while the Mac sleeps; the monotonic clock does not.
    let moment = |seconds, slept| Moment {
        at: after(seconds),
        unix: unix + Duration::from_secs(seconds + slept),
    };
    assert_eq!(health.problem(start), None);
    assert!(health.problem(start + STALLED_AFTER).is_some());

    assert_eq!(health.record(moment(30, 0), &Ok(())), []);
    assert_eq!(health.record(moment(60, 3600), &Ok(())), []);
    assert_eq!(health.problem(after(90)), None);

    let failed = failed("completion cleanup", None, "disk is full");
    let error = "completion cleanup: disk is full";
    assert_eq!(
        health.record(moment(90, 3600), &failed),
        [unreported(None, error)]
    );
    assert_eq!(
        health.record(moment(120, 3600), &failed),
        [unreported(None, error)],
        "an unrecorded notification is retried"
    );
    health.failure.as_mut().unwrap().notified = true;
    assert_eq!(health.record(moment(150, 3600), &failed), []);
    let problem = health.problem(after(150)).unwrap();
    assert!(problem.contains(" 3 "), "{problem}");
    assert!(problem.ends_with(error));

    assert_eq!(health.record(moment(180, 3600), &Ok(())), []);
    assert_eq!(health.problem(after(180)), None);
    assert!(health.problem(after(180) + STALLED_AFTER).is_some());
}

#[test]
fn health_keeps_workspace_failures_to_their_workspace() {
    let mut health = Health::default();
    let start = health.started;
    let moment = |seconds| Moment {
        at: start.at + Duration::from_secs(seconds),
        unix: start.unix + Duration::from_secs(seconds),
    };
    let failed = failed("idle agents", Some("quay"), "bad config");
    let error = "idle agents: bad config";
    assert_eq!(
        health.record(moment(30), &failed),
        [unreported(Some("quay"), error)]
    );
    health.workspaces.get_mut("quay").unwrap().notified = true;
    assert_eq!(health.record(moment(60), &failed), []);

    assert_eq!(health.problem(moment(60).at), None);
    assert_eq!(health.workspace_problem(&SpaceId::from("other")), None);
    let problem = health.workspace_problem(&SpaceId::from("quay")).unwrap();
    assert!(problem.contains(" 2 "), "{problem}");
    assert!(problem.ends_with(error), "{problem}");

    assert_eq!(health.record(moment(90), &Ok(())), []);
    assert_eq!(health.workspace_problem(&SpaceId::from("quay")), None);
}

#[tokio::test]
async fn failing_passes_notify_once_and_show_where_they_belong() {
    use crate::stand_in::repository;
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut workspaces = Vec::new();
    for name in ["failing", "other"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        workspaces.push(workspace);
    }
    let (failing, other) = (&workspaces[0], &workspaces[1]);
    let pass = || {
        let mut failures = SpaceFailures::default();
        failures.push(&failing.id, &failing.name, &anyhow::anyhow!("bad config"));
        let mut failed = SweepFailures::default();
        failed.step("result expiry", Err(anyhow::anyhow!("disk is full")));
        failed.step("idle agents", failures.into_result());
        failed.into_result()
    };
    for _ in 0..2 {
        manager.spaces.record_sweep(&*manager, pass()).await;
    }
    let notifications = manager.spaces.notifications.list(true, 100).await.unwrap();
    let cleanup: Vec<_> = (notifications.iter())
        .filter(|notification| notification.kind == AlertKind::CleanupFailed)
        .map(|notification| notification.space.as_deref())
        .collect();
    assert_eq!(cleanup, [None, Some("failing")]);
    let problem = manager.spaces.cleanup_problem().unwrap();
    assert!(
        problem.ends_with("result expiry: disk is full"),
        "{problem}"
    );
    let status = manager
        .spaces
        .worktree_status(&*manager, &SpaceSelector::id(&failing.id))
        .await
        .unwrap();
    let error = status.cleanup_error.unwrap();
    assert!(error.ends_with("idle agents: bad config"), "{error}");
    let status = manager
        .spaces
        .worktree_status(&*manager, &SpaceSelector::id(&other.id))
        .await
        .unwrap();
    assert_eq!(status.cleanup_error, None);

    manager.spaces.record_sweep(&*manager, Ok(())).await;
    assert_eq!(manager.spaces.cleanup_problem(), None);
    let status = manager
        .spaces
        .worktree_status(&*manager, &SpaceSelector::id(&failing.id))
        .await
        .unwrap();
    assert_eq!(status.cleanup_error, None);
}

#[tokio::test]
async fn idle_sleep_reports_its_failures_as_cleanup_does_under_its_own_name() {
    let manager = Fixture::new().await;
    manager
        .spaces
        .record_idle_sleep(&*manager, failed("idle agents", None, "store is locked"))
        .await;
    let problem = manager.spaces.cleanup_problem().unwrap();
    assert!(problem.starts_with("Idle sleep failed"), "{problem}");
    let notifications = manager.spaces.notifications.list(true, 100).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == AlertKind::CleanupFailed
                && notification.message.starts_with("idle sleep failed")),
        "{notifications:?}"
    );
    // A cleanup sweep that passes leaves the idle sleep's failure.
    manager.spaces.record_sweep(&*manager, Ok(())).await;
    assert!(manager.spaces.cleanup_problem().is_some());
    manager.spaces.record_idle_sleep(&*manager, Ok(())).await;
    assert_eq!(manager.spaces.cleanup_problem(), None);
}
