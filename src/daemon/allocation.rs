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
mod tests;
