//! Arguments of `shoal approvals`.
use clap::{Args, Subcommand, ValueEnum};

use crate::client::space_names::SpaceSelector;
use crate::{cli::args::BodyArgs, client::item_numbers::ItemNumber};

#[derive(Debug, Default, Args)]
pub struct ApprovalsArgs {
    /// Request number, such as 3.2; omit to choose a request.
    pub number: Option<ItemNumber>,
    /// List requests and grants for this space; a chat's are the requests
    /// it decides.
    #[arg(short = 's', long, conflicts_with_all = ["number", "approval_action"])]
    pub space: Option<SpaceSelector>,
    /// Approve the recorded settings, or open a proposed issue or PR.
    #[arg(long, group = "approval_action")]
    pub approve: bool,
    /// Deny the request.
    #[arg(long, group = "approval_action")]
    pub deny: bool,
    /// Pass a request routed to this space up to the space above it or the user.
    #[arg(long, group = "approval_action", requires = "number")]
    pub escalate: bool,
    /// Withdraw a request no longer needed.
    #[arg(long, group = "approval_action", requires = "number")]
    pub withdraw: bool,
    /// Replace a proposed issue's or PR's title.
    #[arg(long, requires = "approve")]
    pub title: Option<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    /// Tell the agent why its proposal was denied.
    #[arg(long, requires = "deny")]
    pub reason: Option<String>,
    /// Deny an issue proposal as a duplicate of this issue number or URL.
    #[arg(long, requires = "deny", conflicts_with = "duplicate_of_approval")]
    pub duplicate_of: Option<String>,
    /// Deny an issue proposal as a duplicate of another pending proposal,
    /// by its number.
    #[arg(long, requires = "deny")]
    pub duplicate_of_approval: Option<ItemNumber>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ApprovalsCommand {
    /// Withdraw a pending request the space's agent no longer needs.
    Withdraw {
        /// Request number, such as 3.2.
        number: ItemNumber,
    },
}

/// What a picker key decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Decision {
    Approve,
    Deny,
}

impl From<Decision> for crate::workers::Decision {
    fn from(decision: Decision) -> Self {
        match decision {
            Decision::Approve => Self::Approve,
            Decision::Deny => Self::Deny,
        }
    }
}
