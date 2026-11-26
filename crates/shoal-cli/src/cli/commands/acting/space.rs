use crate::cli::{args::space::SpaceCommand, commands::spaces, ui::Acting};

pub(super) fn acting(command: &SpaceCommand) -> Option<Acting> {
    Some(match command {
        SpaceCommand::Open { .. } => spaces::opening(),
        // As its dispatch reads it: a name renames the space, and without
        // one only a bare rename picks one to rename; otherwise its agent
        // is asked to name it.
        SpaceCommand::Rename {
            name: Some(_),
            request: false,
            ..
        }
        | SpaceCommand::Rename {
            space: None,
            name: None,
            request: false,
        } => spaces::renaming(),
        SpaceCommand::Rename { .. } => spaces::asking(),
        SpaceCommand::Inbox { .. }
        | SpaceCommand::List { .. }
        | SpaceCommand::Status { .. }
        | SpaceCommand::Log(_) => return None,
    })
}
