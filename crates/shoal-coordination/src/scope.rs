//! Cooperative caller scope: the caller a scope token stands for, the
//! spaces its scope reaches, and what it sees of the rest. The daemon
//! confines every scoped request with them; this is not a security boundary
//! against the OS user.
mod access;
mod reach;
pub mod tokens;

pub use reach::outside_scope;
pub use shoal_daemon_base::callers::{Access, Caller, ScopedExecution};

use anyhow::Result;

/// A refusal by the caller's scope that a request's handler makes, once it
/// knows what the request reaches, which answers as the scope's own
/// refusals do.
#[derive(Debug)]
pub struct ScopeRefusal(anyhow::Error);

impl ScopeRefusal {
    /// `result`, its error a refusal by the caller's scope, except an
    /// approval request and a refusal with a reason of its own, such as a
    /// target that does not exist, which answer as they do anyway.
    pub fn of<T>(result: Result<T>) -> Result<T> {
        result.map_err(|error| {
            let undecided = error.downcast_ref::<crate::approvals::Undecided>();
            match undecided.is_some() || crate::refusal::Reason::of(&error).is_some() {
                true => error,
                false => Self(error).into(),
            }
        })
    }
}

impl std::fmt::Display for ScopeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for ScopeRefusal {}
