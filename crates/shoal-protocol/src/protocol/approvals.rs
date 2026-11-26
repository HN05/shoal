use serde::{Deserialize, Serialize};

use super::{Forwarding, Target};
use crate::item_numbers::ItemNumber;
use crate::space_names::SpaceSelector;

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
        shown: Option<Box<crate::approvals::ApprovalRequest>>,
        /// Edits, a reason or a duplicate, for proposals only.
        #[serde(default)]
        details: crate::approvals::ProposalDecision,
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
        shown: Option<Box<crate::approvals::ApprovalRequest>>,
    },
    /// Withdraw the pending approval request `number` of the space
    /// `space`, which its agent no longer needs.
    WithdrawApproval {
        space: SpaceSelector,
        number: ItemNumber,
    },
    /// Whether the `push` permission holds back the push of the workspace's
    /// branch that `shoal pr open` would run.
    BranchPush {
        workspace: SpaceSelector,
    },
    /// Push `push` from the workspace at once, or record it for the user's
    /// approval; `git push` from an agent asks this.
    RequestPush {
        workspace: SpaceSelector,
        push: crate::approvals::PushSpecification,
    },
    /// Let the caller take `action` itself, as its space's value for the
    /// permission `key` allows. An approval grants a scope token for the
    /// requests the action sends.
    AuthorizeAction {
        key: crate::config::permissions::PermissionKey,
        action: String,
        /// The registered repository the action is in, in whose config a
        /// chat without a repository resolves its permission.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
    },
    /// The push approval `key` started has ended, failed with `failure`
    /// or done; a done push decides the request.
    FinishPush {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<String>,
    },
}

impl ApprovalMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ListApprovals { .. } | Self::ShowApproval { .. } => true,
            Self::DecideApproval { .. }
            | Self::EscalateApproval { .. }
            | Self::WithdrawApproval { .. }
            | Self::BranchPush { .. }
            | Self::RequestPush { .. }
            | Self::AuthorizeAction { .. }
            | Self::FinishPush { .. } => false,
        }
    }
}

impl ApprovalMethod {
    /// A decision goes to the request's machine, waiting for it while it is
    /// offline. An action's authorization and a push's end stay with the
    /// caller.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::DecideApproval { .. } => Forwarding::Queued,
            Self::ListApprovals { .. }
            | Self::ShowApproval { .. }
            | Self::EscalateApproval { .. }
            | Self::WithdrawApproval { .. }
            | Self::BranchPush { .. }
            | Self::RequestPush { .. } => Forwarding::Online,
            Self::AuthorizeAction { .. } | Self::FinishPush { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ShowApproval { number }
            | Self::DecideApproval { number, .. }
            | Self::EscalateApproval { number, .. } => Some(Target::Item(number)),
            Self::ListApprovals { space: Some(space) } | Self::WithdrawApproval { space, .. } => {
                Some(Target::Space(space))
            }
            Self::BranchPush { workspace } | Self::RequestPush { workspace, .. } => {
                Some(Target::Space(workspace))
            }
            Self::ListApprovals { space: None }
            | Self::AuthorizeAction { .. }
            | Self::FinishPush { .. } => None,
        }
    }
}
