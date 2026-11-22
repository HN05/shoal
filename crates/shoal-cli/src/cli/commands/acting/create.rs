//! Commands that start, create or convert a space: the request each sends for the space it acts on.
use crate::cli::{
    args::create::{ConvertCommand, CreateCommand},
    commands::{convert, copies, start},
    ui::Acting,
};

/// What a start, creation or conversion sends for the space it acts on.
pub(super) fn acting(command: &CreateCommand) -> Option<Acting> {
    Some(match command {
        CreateCommand::Start(_) => start::starting(),
        CreateCommand::Create(args) => copies::copying(args.copy.checkpoint),
        CreateCommand::Convert {
            command: ConvertCommand::Workspace { mode, .. },
        } => convert::to_workspace_request(mode.clone()),
        CreateCommand::Convert {
            command: ConvertCommand::Chat { mode, .. },
        } => convert::to_chat_request(mode.clone()),
        // A contribution forks a repository, not a space.
        CreateCommand::Contribute(_) => return None,
    })
}
