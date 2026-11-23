//! The outcome of an allocation the daemon decides: what it granted, the
//! reason it is busy, or the approval request it waits for.
use shoal_protocol::approvals::ApprovalRequest;
use shoal_protocol::protocol::Body;

/// What an allocation came to.
pub enum Allocation<T> {
    Granted(T),
    Busy(String),
    Approval(Box<ApprovalRequest>),
}

impl<T> Allocation<T> {
    /// The answer to the request that asked for it, `granted` making the
    /// body of what it granted.
    pub fn into_body(self, granted: impl FnOnce(T) -> Body) -> Body {
        match self {
            Self::Granted(value) => granted(value),
            Self::Busy(message) => Body::Busy { message },
            Self::Approval(request) => Body::ApprovalRequest(request),
        }
    }
}
