use crate::daemon::allocation::Allocation;
use crate::daemon::approvals::ApprovalRequestExt;
use crate::daemon::approvals::{ApprovalRequest, DecisionStatus, Specification, Target};
use crate::daemon::notifications::AlertKind;
use shoal_protocol::approvals::PortSpecification;
use shoal_protocol::config::repo::Lifetime;

#[tokio::test]
async fn notifications_preserve_busy_wording_and_only_announce_pending_approvals() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    manager
        .leases
        .notify_allocation(
            &*manager,
            (&worker.name, &worker.id),
            &Allocation::Granted(()),
            "",
        )
        .await;
    let port = |name: &str| {
        ApprovalRequest::new(
            &worker.id,
            Target::Port(name.into()),
            name,
            Specification::Port(PortSpecification {
                env: "PORT_WEB".into(),
                on_conflict: Default::default(),
                preferred: None,
                range: [3000, 3100],
            }),
            Lifetime::Lease,
            Some("serve the app"),
        )
    };
    // What was decided before its announcement is not announced.
    for (name, approve) in [("api", true), ("docs", false)] {
        let request = manager
            .coordination
            .request_approval(port(name))
            .await
            .unwrap()
            .unwrap();
        let decided = manager
            .coordination
            .decide_approval(
                &*manager,
                request.number,
                approve,
                None,
                Default::default(),
                None,
            )
            .await
            .unwrap();
        assert_ne!(decided.status, DecisionStatus::Pending);
        manager
            .leases
            .notify_allocation::<()>(
                &*manager,
                (&worker.name, &worker.id),
                &Allocation::Approval(Box::new(request)),
                "",
            )
            .await;
    }
    assert_eq!(manager.notifications.unread().await.unwrap(), 0);

    let request = manager
        .coordination
        .request_approval(port("web"))
        .await
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        for prefix in ["", "simulator: "] {
            manager
                .leases
                .notify_allocation::<()>(
                    &*manager,
                    (&worker.name, &worker.id),
                    &Allocation::Busy("busy".into()),
                    prefix,
                )
                .await;
        }
        manager
            .leases
            .notify_allocation::<()>(
                &*manager,
                (&worker.name, &worker.id),
                &Allocation::Approval(Box::new(request.clone())),
                "",
            )
            .await;
    }
    let notifications = manager.notifications.list(true, 10).await.unwrap();
    assert_eq!(notifications.len(), 3);
    assert!(
        notifications
            .iter()
            .all(|n| n.space.as_deref() == Some("worker"))
    );
    assert_eq!(notifications[0].kind, AlertKind::ResourceBusy);
    assert_eq!(notifications[0].message, "busy");
    assert_eq!(notifications[1].kind, AlertKind::ResourceBusy);
    assert_eq!(notifications[1].message, "simulator: busy");
    assert_eq!(notifications[2].kind, AlertKind::ApprovalRequested);
    let number = request.number.to_string();
    assert!(notifications[2].message.contains(&number));
}
