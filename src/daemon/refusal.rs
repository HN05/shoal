//! Refusals with a typed reason, which the daemon and the client API report
//! as error codes.
//! A refusal displays as its message alone, so the CLI shows what it showed
//! before the reason existed.
use std::fmt;

/// Why the daemon refused an operation, where a caller can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    /// The workspace, repository, link, question or approval does not exist.
    NotFound,
    /// A workspace, an unfinished rename or a running chat has the name or
    /// branch.
    NameTaken,
    /// The workspace is not ready, or its worktree is not verified as its own.
    NotReady,
    /// Removal would delete unsaved work the caller did not confirm.
    UnsavedWork,
}

#[derive(Debug)]
pub(crate) struct Refusal {
    pub reason: Reason,
    message: String,
}

impl Refusal {
    pub(crate) fn new(reason: Reason, message: impl fmt::Display) -> Self {
        Self {
            reason,
            message: message.to_string(),
        }
    }

    /// `error` refused for `reason`, keeping its message.
    pub(crate) fn wrap(reason: Reason, error: anyhow::Error) -> anyhow::Error {
        Self::new(reason, format!("{error:#}")).into()
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

impl Reason {
    /// The code the daemon answers a refusal for this reason with.
    pub(crate) fn code(self) -> crate::protocol::ErrorCode {
        use crate::protocol::ErrorCode;
        match self {
            Self::NotFound => ErrorCode::NotFound,
            Self::NameTaken => ErrorCode::NameTaken,
            Self::NotReady => ErrorCode::NotReady,
            Self::UnsavedWork => ErrorCode::UnsavedWork,
        }
    }

    /// The reason of the first refusal in `error`'s chain.
    pub(crate) fn of(error: &anyhow::Error) -> Option<Self> {
        error.chain().find_map(|cause| {
            if let Some(refusal) = cause.downcast_ref::<Refusal>() {
                return Some(refusal.reason);
            }
            cause
                .downcast_ref::<crate::repository::NotRegistered>()
                .map(|_| Self::NotFound)
        })
    }
}

#[cfg(test)]
mod tests;
