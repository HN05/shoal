use serde::{Deserialize, Serialize};

use crate::client::item_numbers::ItemNumber;
use crate::client::space_names::SpaceSelector;

/// Requests about approval requests and their decisions.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMethod {
    ListApprovals {
        space: Option<SpaceSelector>,
    },
    ShowApproval {
        number: ItemNumber,
    },
    /// `shown` is the request the user was shown, which a later request
    /// reusing its number does not match.
    DecideApproval {
        number: ItemNumber,
        approve: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<Box<crate::client::approvals::ApprovalRequest>>,
        /// Edits, a reason or a duplicate, for proposals only.
        #[serde(default)]
        details: crate::client::approvals::ProposalDecision,
        /// The environment approving a push pushes in, the user's without
        /// its secrets; the daemon's without it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<std::collections::BTreeMap<String, String>>,
    },
    /// Pass a request the space `space` decides on to the space above it, or
    /// to the user.
    EscalateApproval {
        space: SpaceSelector,
        number: ItemNumber,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<Box<crate::client::approvals::ApprovalRequest>>,
    },
    /// Withdraw the pending approval request `number` of the space
    /// `space`, which its agent no longer needs.
    WithdrawApproval {
        space: SpaceSelector,
        number: ItemNumber,
    },
    /// Whether `[permissions] push` holds back the push of the workspace's
    /// branch that `shoal pr open` would run.
    BranchPush {
        workspace: SpaceSelector,
    },
    /// Push `push` from the workspace at once, or record it for the user's
    /// approval; `git push` from an agent asks this.
    RequestPush {
        workspace: SpaceSelector,
        push: crate::client::approvals::PushSpecification,
    },
    /// The push approval `key` started has ended, failed with `failure`
    /// or done; a done push decides the request.
    FinishPush {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<String>,
    },
}
