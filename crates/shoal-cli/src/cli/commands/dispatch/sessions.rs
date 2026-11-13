use anyhow::Result;

use crate::cli::{
    args::sessions::SessionsCommand,
    commands::{
        demo, queue, restart,
        restore::{self, agents},
        sessions,
    },
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
        SessionsCommand::Restore {
            targets,
            checkpoint,
            execution,
            forget,
            now,
            focus,
            permissions,
            queue_entry,
            opened_in,
            close_in,
        } => {
            if let Some(host) = opened_in {
                agents::queued::record_place(&ctx, host, &targets).await;
            }
            if let Some(host) = close_in {
                agents::queued::close_place_when_done(&ctx, host, &targets).await;
            }
            match queue_entry {
                None => {
                    let options = agents::Options {
                        execution,
                        forget,
                        now,
                        focus,
                        change: change(&permissions),
                    };
                    restore::run(&ctx, targets, checkpoint, options).await
                }
                Some(entry) => agents::queued::start_entry(&ctx, targets, execution, entry).await,
            }
        }
        SessionsCommand::Restart {
            targets,
            agent,
            wait,
            sequential,
            in_place,
            permissions,
        } => {
            let options = crate::client::restarts::Options {
                wait,
                sequential,
                in_place,
                change: change(&permissions),
            };
            restart::run(&ctx, targets, agent, options).await
        }
        SessionsCommand::Queue {
            command,
            repository,
            print,
        } => queue::run(&ctx, command, repository, print).await,
    }
}

/// What the `--permission` assignments of a restore or restart change in the
/// permissions the agents were given; none without any, which keeps them.
fn change(changes: &[PermissionChange]) -> Option<PermissionChange> {
    (!changes.is_empty()).then(|| PermissionChange::combined(changes))
}
