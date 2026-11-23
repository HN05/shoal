use anyhow::Result;

use crate::cli::{
    args::sessions::SessionsCommand,
    commands::{demo, pause, queue, restart, restore, resume, sessions, triggers},
    context::Context,
};
use crate::config::permissions::PermissionChange;

pub(in crate::cli::commands) async fn run(ctx: Context, command: SessionsCommand) -> Result<i32> {
    match command {
        SessionsCommand::Attach { target, session } => {
            sessions::attach(&ctx, target, session).await
        }
        SessionsCommand::Demo {
            command,
            name,
            workspace,
        } => demo::run(&ctx, command, name, workspace).await,
        SessionsCommand::Resume {
            targets,
            execution,
            now,
            focus,
            permissions,
            queue_entry,
            opened_in,
            close_in,
        } => {
            if let Some(host) = opened_in {
                resume::queued::record_place(&ctx, host, &targets).await;
            }
            if let Some(host) = close_in {
                resume::queued::close_place_when_done(&ctx, host, &targets).await;
            }
            match queue_entry {
                None => {
                    let options = resume::Options {
                        execution,
                        now,
                        focus,
                        change: change(&permissions),
                    };
                    resume::run(&ctx, targets, options).await
                }
                Some(entry) => resume::queued::start_entry(&ctx, targets, execution, entry).await,
            }
        }
        SessionsCommand::Restore {
            import, no_resume, ..
        } if !import.from.is_empty() => {
            let options = restore::import::ImportOptions {
                sessions: import.sessions(),
                repository: import.repository,
                restore: !no_resume,
            };
            restore::import::import(&ctx, import.from, options).await
        }
        SessionsCommand::Restore {
            space,
            all,
            import: _,
            checkpoint,
            discard_agent,
            execution,
            no_resume,
            now,
            focus,
            permissions,
        } => {
            let action = match (checkpoint, discard_agent, no_resume) {
                (Some(checkpoint), ..) => restore::Action::Checkpoint(checkpoint),
                (None, true, _) => restore::Action::DiscardAgent { all, execution },
                (None, false, true) => restore::Action::Reopen { resume: None },
                (None, false, false) => {
                    let options = resume::Options {
                        execution: None,
                        now,
                        focus,
                        change: change(&permissions),
                    };
                    restore::Action::Reopen {
                        resume: Some(options),
                    }
                }
            };
            restore::run(&ctx, space, action).await
        }
        SessionsCommand::Restart {
            targets,
            machines,
            cancel: true,
            ..
        } => restart::cancel(&ctx, targets, &machines).await,
        SessionsCommand::Restart {
            targets,
            machines,
            agent,
            wait,
            sequential,
            in_place,
            permissions,
            cancel: false,
        } => {
            let restart = crate::model::AgentRestart {
                wait,
                sequential,
                in_place,
                change: change(&permissions),
            };
            restart::run(&ctx, targets, &machines, agent, restart).await
        }
        SessionsCommand::Queue {
            command,
            repository,
            print,
        } => queue::run(&ctx, command, repository, print).await,
        SessionsCommand::Trigger { command } => triggers::run(&ctx, command).await,
        SessionsCommand::Pause { machine } => pause::pause(&ctx, machine).await,
        SessionsCommand::Unpause { machine } => pause::unpause(&ctx, machine).await,
    }
}

/// What the `--permission` assignments of a restore or restart change in the
/// permissions the agents were given; none without any, which keeps them.
fn change(changes: &[PermissionChange]) -> Option<PermissionChange> {
    (!changes.is_empty()).then(|| PermissionChange::combined(changes))
}
