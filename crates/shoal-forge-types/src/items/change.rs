//! The changes Shoal makes to an issue, PR or CI run, as a space's log
//! records them.

use shoal_states::states;

use crate::ci::CiAction;

states!(
    /// A change Shoal made to an item: an [`Action`](crate::items::action::Action),
    /// a CI action, opening the item or approving its waiting runs.
    ItemChange {
        Open => "open",
        Edit => "edit",
        Comment => "comment",
        EditComment => "edit_comment",
        Close => "close",
        Reopen => "reopen",
        Merge => "merge",
        AutoMerge => "auto_merge",
        CancelAutoMerge => "cancel_auto_merge",
        Update => "update",
        Reply => "reply",
        Resolve => "resolve",
        Unresolve => "unresolve",
        Review => "review",
        /// A review that approves a PR, or the approval of waiting CI runs.
        Approve => "approve",
        RequestChanges => "request_changes",
        React => "react",
        Unreact => "unreact",
        Rerun => "rerun",
        Cancel => "cancel",
    }
);

impl ItemChange {
    /// The change as an error names it, such as "request changes".
    pub fn verb(self) -> String {
        self.as_str().replace('_', " ")
    }
}

impl From<CiAction> for ItemChange {
    fn from(action: CiAction) -> Self {
        match action {
            CiAction::Rerun => Self::Rerun,
            CiAction::Cancel => Self::Cancel,
        }
    }
}

#[cfg(test)]
mod tests;
