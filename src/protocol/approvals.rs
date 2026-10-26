use serde::{Deserialize, Serialize};

/// Requests about approval requests and their decisions.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMethod {
    ListApprovals {
        workspace: Option<String>,
    },
    ShowApproval {
        id: i64,
    },
    /// `shown` is the request the user was shown, which a later request
    /// reusing its number does not match.
    DecideApproval {
        id: i64,
        approve: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<Box<crate::daemon::approvals::ApprovalRequest>>,
        /// Edits, a reason or a duplicate, for proposals only.
        #[serde(default)]
        details: crate::daemon::approvals::proposals::ProposalDecision,
    },
    /// Pass a request the coordinating agent of `workspace` decides on to the
    /// next coordinating agent above it that holds a grant, or to the user.
    EscalateApproval {
        workspace: String,
        id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<Box<crate::daemon::approvals::ApprovalRequest>>,
    },
    /// Withdraw the workspace's pending approval request `id`, which its
    /// agent no longer needs.
    WithdrawApproval {
        workspace: String,
        id: i64,
    },
}
