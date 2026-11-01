//! Shared outcomes of daemon-owned resource allocation.
use crate::{
    daemon::{approvals::ApprovalRequest, notifications::NotificationKind, workspace::Manager},
    protocol::Body,
};

pub enum Allocation<T> {
    Granted(T),
    Busy(String),
    Approval(Box<ApprovalRequest>),
}

impl<T> Allocation<T> {
    pub fn into_body(self, granted: impl FnOnce(T) -> Body) -> Body {
        match self {
            Self::Granted(value) => granted(value),
            Self::Busy(message) => Body::Busy { message },
            Self::Approval(request) => Body::ApprovalRequest(request),
        }
    }
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
