//! Whether an inspected workspace waits for a retried setup, as the agent
//! its start queued does.
use crate::state::WorkspaceState;
use shoal_protocol::status::Inspection;

/// Whether the inspected workspace waits for a retried setup: its setup
/// failed, or never started and runs in no execution, as when the daemon
/// refused it.
pub(in crate::cli::commands) fn awaits_setup(inspection: &Inspection) -> bool {
    match inspection.workspace.state {
        WorkspaceState::Failed => true,
        WorkspaceState::Preparing => inspection.executions.is_empty(),
        _ => false,
    }
}
