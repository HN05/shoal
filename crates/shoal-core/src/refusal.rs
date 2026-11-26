//! Refusals with a typed reason, which the daemon reports as error codes. A
//! refusal displays as its message alone.
use std::fmt;

use shoal_protocol::space_names::SpaceSelector;

/// Why the daemon refused an operation, where a caller can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The workspace, repository, link, question or approval does not exist.
    NotFound,
    /// A workspace, an unfinished rename or a running chat has the name or
    /// branch.
    NameTaken,
    /// The workspace is not ready, or its worktree is not verified as its own.
    NotReady,
    /// Removal would delete unsaved work the caller did not confirm.
    UnsavedWork,
    /// An import failed before it recorded anything of the space.
    NotImported,
    /// The other joined machine a request goes to cannot be reached through
    /// the hub now, or its answer broke off.
    MachineUnreachable,
    /// A write or deletion of an app's entry names a revision the entry is
    /// no longer at.
    StaleRevision,
    /// A move of the space is recorded, which its worker or Shoal finishes.
    MoveInProgress,
}

#[derive(Debug)]
pub struct Refusal {
    pub(crate) reason: Reason,
    message: String,
    /// The space the request named that this machine has none of, which
    /// another joined machine may have.
    absent: Option<SpaceSelector>,
    /// The typed error the refusal reports, which callers tell apart.
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl Refusal {
    pub fn new(reason: Reason, message: impl fmt::Display) -> Self {
        Self {
            reason,
            message: message.to_string(),
            absent: None,
            cause: None,
        }
    }

    /// `cause` refused for `reason`, showing its message; [`Self::cause`]
    /// finds it again.
    pub(crate) fn because(
        reason: Reason,
        cause: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        let refusal = Self::new(reason, &cause);
        Self {
            cause: Some(Box::new(cause)),
            ..refusal
        }
    }

    /// The refusal of a request for the space `selector` names, which this
    /// machine has none of.
    pub fn absent(selector: &SpaceSelector, message: impl fmt::Display) -> Self {
        Self {
            absent: Some(selector.clone()),
            ..Self::new(Reason::NotFound, message)
        }
    }

    /// The space this machine has none of that the first refusal in
    /// `error`'s chain names, if it names one.
    pub fn absent_space(error: &anyhow::Error) -> Option<&SpaceSelector> {
        let refusal = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<Self>())?;
        refusal.absent.as_ref()
    }

    /// The cause of type `T` that the first refusal in `error`'s chain
    /// reports, if it reports one.
    pub(crate) fn cause<T: std::error::Error + 'static>(error: &anyhow::Error) -> Option<&T> {
        let refusal = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<Self>())?;
        refusal.cause.as_ref()?.downcast_ref()
    }

    /// `error` refused for `reason`, keeping its message.
    pub fn wrap(reason: Reason, error: anyhow::Error) -> anyhow::Error {
        Self::new(reason, format!("{error:#}")).into()
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// Refuse a decision on `record`, which changed since its decider was shown
/// it, as gone: a later record reusing its number is another one.
pub fn ensure_unchanged(same: bool, record: impl fmt::Display) -> anyhow::Result<()> {
    if same {
        return Ok(());
    }
    let message = format!("{record} changed since it was shown; choose again");
    Err(Refusal::new(Reason::NotFound, message).into())
}

impl Reason {
    /// The code the daemon answers a refusal for this reason with.
    pub fn code(self) -> crate::protocol::ErrorCode {
        use crate::protocol::ErrorCode;
        match self {
            Self::NotFound => ErrorCode::NotFound,
            Self::NameTaken => ErrorCode::NameTaken,
            Self::NotReady => ErrorCode::NotReady,
            Self::UnsavedWork => ErrorCode::UnsavedWork,
            Self::NotImported => ErrorCode::NotImported,
            Self::MachineUnreachable => ErrorCode::MachineUnreachable,
            Self::StaleRevision => ErrorCode::StaleRevision,
            Self::MoveInProgress => ErrorCode::MoveInProgress,
        }
    }

    /// The reason of the first refusal in `error`'s chain.
    pub fn of(error: &anyhow::Error) -> Option<Self> {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<Refusal>())
            .map(|refusal| refusal.reason)
    }
}

#[cfg(test)]
mod tests;
