//! What a read-only scope may request: what only reads the spaces it
//! reaches, but not a read that marks what it read or replaces an agent's
//! watch, hands out a login, saves a file, or checks a change before the
//! client makes it.
use super::Scoped;
use crate::protocol::{
    AgentMethod, ChatMethod, ExecutionMethod, ItemMethod, NotificationMethod, Operation,
    RemovalMethod, WorkspaceMethod,
};

/// Whether a read-only scope may send `operation`.
pub(super) fn allows(operation: &Operation) -> bool {
    operation.reads_space() && !takes_more_than_it_reads(operation)
}

/// The requests that only read state, which a read-only scope still may
/// not send.
fn takes_more_than_it_reads(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Agent(AgentMethod::AgentUpdates { .. } | AgentMethod::GitCredential { .. })
            | Operation::Notification(NotificationMethod::ListNotifications { .. })
            | Operation::Item(ItemMethod::ForgeDownload { .. })
            | Operation::Chat(ChatMethod::CheckChatParent { .. })
            | Operation::Execution(ExecutionMethod::CheckMerge | ExecutionMethod::CheckUnmerge)
            | Operation::Removal(
                RemovalMethod::CheckRemoval { .. } | RemovalMethod::CheckStop { .. }
            )
            | Operation::Workspace(
                WorkspaceMethod::CheckCreation { .. } | WorkspaceMethod::CheckBlockers { .. }
            )
    )
}

#[cfg(test)]
mod tests;
