use crate::cli::{args::approvals::ApprovalsCommand, ui::Acting};

pub(super) fn acting(command: Option<&ApprovalsCommand>) -> Option<Acting> {
    match command {
        None | Some(ApprovalsCommand::Withdraw { .. }) => None,
    }
}
