use anyhow::Result;

use crate::cli::{
    args::sessions::SessionsCommand,
    commands::{demo, restart, resume, sessions},
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
        } => resume::select(&ctx, targets, execution, discard).await,
        SessionsCommand::Restart {
            targets,
            agent,
            wait,
            sequential,
        } => restart::run(&ctx, targets, agent, wait, sequential).await,
    }
}
