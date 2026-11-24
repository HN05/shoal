//! What a read-only scope may request: what only reads the spaces it
//! reaches, but not a read that marks what it read or replaces an agent's
//! watch, hands out a login, saves a file, or checks a change before the
//! client makes it; and what a review run's scope may send besides.
use super::Scoped;
use crate::protocol::{
    AgentMethod, ExecutionMethod, ItemMethod, LaunchMethod, Operation, RemovalMethod, ReviewMethod,
    ThreadMethod,
};

/// Whether a read-only scope may send `operation`.
pub(super) fn allows(operation: &Operation) -> bool {
    operation.reads_space() && !takes_more_than_it_reads(operation)
}

/// Whether a review run's scope may send `operation` beside what a
/// read-only scope may: a thread without a kind, which opens a finding, a
/// post, which reopens one, and a submit, each of which the daemon hands
/// its review.
pub(super) fn writes_review(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Thread(ThreadMethod::OpenThread { kind: None, .. } | ThreadMethod::Post { .. })
            | Operation::Review(ReviewMethod::SubmitReview { .. })
    )
}

/// The requests that only read state, which a read-only scope still may
/// not send.
fn takes_more_than_it_reads(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Agent(AgentMethod::AgentUpdates { .. } | AgentMethod::GitCredential { .. })
            | Operation::Item(ItemMethod::ForgeDownload { .. })
            | Operation::Launch(
                LaunchMethod::CheckLaunch { .. }
                    | LaunchMethod::CheckCreation { .. }
                    | LaunchMethod::LaunchModes { .. }
                    | LaunchMethod::LabelledMode { .. }
            )
            | Operation::Execution(ExecutionMethod::CheckMerge | ExecutionMethod::CheckUnmerge)
            | Operation::Removal(
                RemovalMethod::CheckRemoval { .. } | RemovalMethod::CheckStop { .. }
            )
    )
}

#[cfg(test)]
mod tests;
