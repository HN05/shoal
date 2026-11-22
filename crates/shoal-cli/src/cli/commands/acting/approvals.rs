//! Approval commands, which name requests by number, not spaces.
use crate::cli::{args::approvals::ApprovalsCommand, ui::Acting};

/// What an approval command sends for the space it acts on: none.
pub(super) fn acting(command: Option<&ApprovalsCommand>) -> Option<Acting> {
    match command {
        None | Some(ApprovalsCommand::Withdraw { .. }) => None,
    }
}
