//! Shared outcomes of daemon-owned resource allocation.
use crate::{Leases, daemon::Daemon, protocol::Body};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::ApprovalRequest;
use shoal_protocol::space_names::SpaceName;

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

impl Leases {
    /// Record allocation notifications after the claim transaction has finished.
    pub async fn notify_allocation<T>(
        &self,
        daemon: &dyn Daemon,
        space: (&SpaceName, &SpaceId),
        outcome: &Allocation<T>,
        busy_prefix: &str,
    ) {
        match outcome {
            Allocation::Granted(_) => {}
            Allocation::Busy(message) => {
                let message = format!("{busy_prefix}{message}");
                (self.notify(daemon, space, AlertKind::ResourceBusy, message)).await;
            }
            Allocation::Approval(request) => daemon.announce(request).await,
        }
    }
}
