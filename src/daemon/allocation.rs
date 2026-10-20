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
        let (_temp, manager) = crate::test_support::manager().await;
        manager
            .notify_allocation("worker", &Allocation::Granted(()), "")
            .await;
        let mut request = ApprovalRequest::new(
            "owner",
            Target::Port("web".into()),
            "web",
            Specification::Port(PortSpecification {
                env: "PORT_WEB".into(),
                on_conflict: Default::default(),
                preferred: None,
                range: [3000, 3100],
            }),
            Lifetime::Lease,
            Some("serve the app"),
        );
        request.id = 7;
        for status in [DecisionStatus::Approved, DecisionStatus::Denied] {
            request.status = status;
            manager
                .notify_allocation::<()>(
                    "worker",
                    &Allocation::Approval(Box::new(request.clone())),
                    "",
                )
                .await;
        }
        assert_eq!(manager.unread_notifications().await.unwrap(), 0);

        request.status = DecisionStatus::Pending;
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
