//! Workspace commands: the request each sends for the space it acts on.
use crate::cli::{
    args::workspace::WorkspaceCommand,
    commands::{cd, edit, refresh, restore, setup},
    ui::Acting,
};

/// What a workspace command sends for the space it acts on.
pub(super) fn acting(command: &WorkspaceCommand) -> Option<Acting> {
    Some(match command {
        WorkspaceCommand::Cd { .. } => cd::entering(),
        WorkspaceCommand::Edit { .. } => edit::editing(),
        WorkspaceCommand::Setup { skip, .. } => setup::setting_up(*skip),
        WorkspaceCommand::Refresh { .. } => refresh::refreshing(),
        WorkspaceCommand::Checkpoint { .. } => restore::taking(),
        // An adoption names a directory, not a space.
        WorkspaceCommand::Adopt { .. } => return None,
    })
}
