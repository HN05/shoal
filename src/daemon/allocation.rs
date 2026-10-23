//! Shared outcomes of daemon-owned resource allocation.
use crate::daemon::{
    approvals::ApprovalRequest, notifications::NotificationKind, workspace::Manager,
};

pub enum Allocation<T> {
    Granted(T),
    Busy(String),
    Approval(Box<ApprovalRequest>),
}

impl Manager {
    /// Record allocation notifications after the claim transaction has finished.
    pub async fn notify_allocation<T>(
        &self,
        workspace: &str,
        outcome: &Allocation<T>,
        busy_prefix: &str,
    ) {
        match outcome {
            Allocation::Granted(_) => {}
            Allocation::Busy(message) => {
                self.notify(
                    Some(workspace),
                    NotificationKind::ResourceBusy,
                    format!("{busy_prefix}{message}"),
                )
                .await;
            }
            Allocation::Approval(request) => self.notify_approval(workspace, request).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::approvals::{
        DecisionStatus, Lifetime, PortSpecification, Specification, Target,
    };

    #[tokio::test]
    async fn notifications_preserve_busy_wording_and_only_announce_pending_approvals() {
        let (temp, manager) = crate::test_support::manager().await;
        let path = crate::test_support::repository(temp.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let worker = manager
            .create_workspace(&repo.id, "worker".into(), None, None, None)
            .await
            .unwrap();
        manager
            .notify_allocation("worker", &Allocation::Granted(()), "")
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
            let request = manager.request_approval(port(name)).await.unwrap().unwrap();
            let decided = manager
                .decide_approval(request.id, approve, None, Default::default())
                .await
                .unwrap();
            assert_ne!(decided.status, DecisionStatus::Pending);
            manager
                .notify_allocation::<()>("worker", &Allocation::Approval(Box::new(request)), "")
                .await;
        }
        assert_eq!(manager.unread_notifications().await.unwrap(), 0);

        let request = manager
            .request_approval(port("web"))
            .await
            .unwrap()
            .unwrap();
        for _ in 0..2 {
            for prefix in ["", "simulator: "] {
                manager
                    .notify_allocation::<()>("worker", &Allocation::Busy("busy".into()), prefix)
                    .await;
            }
            manager
                .notify_allocation::<()>(
                    "worker",
                    &Allocation::Approval(Box::new(request.clone())),
                    "",
                )
                .await;
        }
        let notifications = manager.notifications(true, 10).await.unwrap();
        assert_eq!(notifications.len(), 3);
        assert!(
            notifications
                .iter()
                .all(|n| n.workspace.as_deref() == Some("worker"))
        );
        assert_eq!(notifications[0].kind, NotificationKind::ResourceBusy);
        assert_eq!(notifications[0].message, "busy");
        assert_eq!(notifications[1].kind, NotificationKind::ResourceBusy);
        assert_eq!(notifications[1].message, "simulator: busy");
        assert_eq!(notifications[2].kind, NotificationKind::ApprovalRequested);
        assert_eq!(
            notifications[2].message,
            format!(
                "approval {}: port/web / web (lease): serve the app; approve with shoal approve {}",
                request.id, request.id
            )
        );
    }
}
