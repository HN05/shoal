use anyhow::Result;

use crate::cli::{
    args::sessions::SessionsCommand,
    commands::{demo, queue, restart, resume, sessions},
    context::Context,
};
use crate::config::permissions::PermissionsConfig;

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
            discard,
            now,
            focus,
            narrowing,
            queue_entry,
            opened_in,
            close_in,
        } => {
            if let Some(host) = opened_in {
                resume::record_place(&ctx, host, &targets).await;
            }
            if let Some(host) = close_in {
                resume::close_place_when_done(&ctx, host, &targets).await;
            }
            match queue_entry {
                None => {
                    let options = resume::Options {
                        execution,
                        discard,
                        now,
                        focus,
                        narrowing: replacement(&narrowing),
                    };
                    resume::select(&ctx, targets, options).await
                }
                Some(entry) => resume::start_entry(&ctx, targets, execution, entry).await,
            }
        }
        SessionsCommand::Restart {
            targets,
            agent,
            wait,
            sequential,
            in_place,
            narrowing,
        } => {
            let options = restart::Options {
                wait,
                sequential,
                in_place,
                narrowing: replacement(&narrowing),
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

/// The narrowing the `--permission` assignments of a resume or restart
/// give, which replaces the recorded one; none without any, which keeps it.
fn replacement(narrowings: &[PermissionsConfig]) -> Option<PermissionsConfig> {
    (!narrowings.is_empty()).then(|| PermissionsConfig::combined(narrowings))
}
