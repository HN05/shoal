use crate::cli::{args::internal::InternalCommand, commands::environment, ui::Acting};

/// Only exporting a space's environment picks a space.
pub(super) fn acting(command: &InternalCommand) -> Option<Acting> {
    match command {
        InternalCommand::Env { revoke, .. } => Some(environment::exporting(revoke.clone())),
        InternalCommand::Events { .. }
        | InternalCommand::MenuRows { .. }
        | InternalCommand::MenuNext
        | InternalCommand::ApprovalRows { .. }
        | InternalCommand::ApprovalDecide { .. }
        | InternalCommand::ProxyReport { .. }
        | InternalCommand::AgentState { .. }
        | InternalCommand::TurnFailed { .. }
        | InternalCommand::Plan { .. }
        | InternalCommand::Change { .. }
        | InternalCommand::Deliver
        | InternalCommand::Wake { .. }
        | InternalCommand::SubagentStart
        | InternalCommand::Subagent { .. }
        | InternalCommand::WorktreeCreate
        | InternalCommand::WorktreeRemove
        | InternalCommand::Merge { .. }
        | InternalCommand::Unmerge { .. }
        | InternalCommand::PlaceWatch { .. }
        | InternalCommand::HerdrWatch { .. }
        | InternalCommand::Detached { .. }
        | InternalCommand::SecretTool { .. }
        | InternalCommand::PushGate { .. }
        | InternalCommand::GitCredential { .. }
        | InternalCommand::Operation
        | InternalCommand::ImportMoved { .. }
        | InternalCommand::PlaceSessions { .. }
        | InternalCommand::Attach { .. }
        | InternalCommand::ShowDemo { .. }
        | InternalCommand::QueueStart { .. }
        | InternalCommand::Place(_)
        | InternalCommand::Session { .. }
        | InternalCommand::Demo { .. }
        | InternalCommand::Launch { .. }
        | InternalCommand::TakeOver { .. }
        | InternalCommand::CopyIgnored { .. } => None,
    }
}
