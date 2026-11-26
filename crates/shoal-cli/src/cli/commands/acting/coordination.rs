use crate::cli::{
    args::{
        coordination::{AlertCommand, ChangeCommand, CoordinationCommand, FilesCommand},
        threads::ThreadCommand,
    },
    commands::{files, priority, progress, send, sleep, spaces, threads},
    ui::Acting,
};

pub(super) fn acting(command: &CoordinationCommand) -> Option<Acting> {
    Some(match command {
        CoordinationCommand::Send { in_chat: true, .. } => send::marking(),
        CoordinationCommand::Send { .. } => send::alerting(),
        CoordinationCommand::Thread { command } => return thread(command),
        CoordinationCommand::Files {
            command: Some(FilesCommand::Add { .. }),
            ..
        } => files::adding(),
        CoordinationCommand::Files {
            command: Some(FilesCommand::Rm { .. }),
            ..
        } => files::removing(),
        CoordinationCommand::Files { command: None, .. } => files::showing(),
        CoordinationCommand::Progress {
            status,
            steps,
            add_blocker,
            remove_blocker,
            ..
        } => {
            let blocks = add_blocker.is_some() || remove_blocker.is_some();
            progress::reporting(status.is_some() || *steps || blocks)
        }
        CoordinationCommand::Sleep { .. } => sleep::sleeping(),
        CoordinationCommand::Priority { priority, .. } => priority::setting(*priority),
        CoordinationCommand::Done { undo: true, .. } => spaces::undoing(),
        CoordinationCommand::Done { .. } => spaces::marking(),
        // Changes and alerts are the caller's own or picked by number.
        CoordinationCommand::Changes {
            command: None | Some(ChangeCommand::Claim { .. } | ChangeCommand::Unclaim { .. }),
            ..
        }
        | CoordinationCommand::Alert {
            command: AlertCommand::List(_) | AlertCommand::Withdraw { .. } | AlertCommand::Clear,
        } => return None,
    })
}

/// Only opening a thread picks the space it goes to.
fn thread(command: &ThreadCommand) -> Option<Acting> {
    match command {
        ThreadCommand::Open { kind, .. } => Some(threads::opening(*kind)),
        ThreadCommand::Forward { .. }
        | ThreadCommand::Resolve { .. }
        | ThreadCommand::Switch { .. }
        | ThreadCommand::Add { .. }
        | ThreadCommand::Remove { .. }
        | ThreadCommand::List { .. } => None,
    }
}
