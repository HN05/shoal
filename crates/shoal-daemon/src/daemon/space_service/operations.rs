//! How the spaces check an operation's target and answer its failure.
use shoal_protocol::protocol::{Body, Operation};

use crate::daemon::manager::Manager;
use shoal_spaces::target_checks::TargetCheck;

impl shoal_spaces::daemon::Operations for Manager {
    fn target_check(&self, operation: &Operation) -> Option<TargetCheck> {
        crate::daemon::target_checks::Checked::target_check(operation)
    }

    fn failure(&self, error: &anyhow::Error) -> Body {
        crate::daemon::answer::failure(error)
    }
}
