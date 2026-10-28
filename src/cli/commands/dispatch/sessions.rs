use anyhow::Result;

use crate::cli::{
    args::sessions::SessionsCommand,
    commands::{demo, queue, restart, resume, sessions},
    context::Context,
};

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
            queue_entry: None,
        } => resume::select(&ctx, targets, execution, discard, now).await,
        SessionsCommand::Resume {
            targets,
            execution,
            queue_entry: Some(entry),
            ..
        } => resume::start_entry(&ctx, targets, execution, entry).await,
        SessionsCommand::Restart {
            targets,
            agent,
            wait,
            sequential,
        } => restart::run(&ctx, targets, agent, wait, sequential).await,
        SessionsCommand::Queue {
            command,
            repository,
            print,
        } => queue::run(&ctx, command, repository, print).await,
    }
}
