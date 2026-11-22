//! Agent session commands: the request each sends for the space it acts on.
use crate::cli::{
    args::sessions::{QueueCommand, SessionsCommand, TriggerCommand},
    commands::{demo, restart, restore, sessions},
    ui::Acting,
};

/// What an agent session command sends for the space it acts on.
pub(super) fn acting(command: &SessionsCommand) -> Option<Acting> {
    Some(match command {
        SessionsCommand::Attach { .. } => sessions::attaching(),
        SessionsCommand::Demo { command, .. } => return demo::acting(command.as_ref()),
        SessionsCommand::Restore {
            checkpoint: Some(_),
            ..
        } => restore::returning(),
        SessionsCommand::Restart { cancel: true, .. } => restart::cancelling(),
        SessionsCommand::Restart { .. } => restart::restarting(),
        // A resume and a reopening offer what was saved or removed, which
        // completion lists apart.
        SessionsCommand::Resume { .. } | SessionsCommand::Restore { .. } => return None,
        SessionsCommand::Queue {
            command: None | Some(QueueCommand::Start { .. } | QueueCommand::Cancel { .. }),
            ..
        }
        | SessionsCommand::Trigger {
            command:
                TriggerCommand::Ls { .. }
                | TriggerCommand::Pause { .. }
                | TriggerCommand::Resume { .. }
                | TriggerCommand::Run { .. },
        } => return None,
    })
}
