//! Arguments of `shoal approvals`.
use clap::{Args, Subcommand, ValueEnum};

use crate::cli::args::BodyArgs;

#[derive(Debug, Default, Args)]
pub struct ApprovalsArgs {
    /// Request number; omit to choose a request.
    pub id: Option<i64>,
    /// List requests and grants for this workspace.
    #[arg(long, conflicts_with_all = ["id", "approval_action"])]
    pub workspace: Option<String>,
    /// Approve the recorded settings, or open a proposed issue or PR.
    #[arg(long, group = "approval_action")]
    pub approve: bool,
    /// Deny the request.
    #[arg(long, group = "approval_action")]
    pub deny: bool,
    /// Pass a request routed to this workspace's coordinator up to its parent or the user.
    #[arg(long, group = "approval_action", requires = "id")]
    pub escalate: bool,
    /// Withdraw a request no longer needed.
    #[arg(long, group = "approval_action", requires = "id")]
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
    /// Deny an issue proposal as a duplicate of another pending proposal.
    #[arg(long, requires = "deny")]
    pub duplicate_of_approval: Option<i64>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ApprovalsCommand {
    /// Withdraw a pending request the workspace's agent no longer needs.
    Withdraw {
        /// Request number.
        id: i64,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
}

/// What a picker key decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Decision {
    Approve,
    Deny,
}
