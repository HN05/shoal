//! Who a caller assigns an issue to a space as.
use crate::{daemon::scope::Caller, model::Assigner};

/// Who a caller assigns an issue to a space as.
pub(crate) trait AssignerOf {
    /// An agent for a scoped `caller`, else the user.
    fn of(caller: Option<&Caller>) -> Self;
}

impl AssignerOf for Assigner {
    fn of(caller: Option<&Caller>) -> Self {
        match caller {
            Some(_) => Self::Agent,
            None => Self::User,
        }
    }
}

#[cfg(test)]
mod tests;
