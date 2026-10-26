use anyhow::Result;

use crate::cli::{
    args::repository::{ForgeCommand, RepositoryCommand},
    commands::{api, capabilities, repositories, workflows},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: RepositoryCommand) -> Result<i32> {
    match command {
        RepositoryCommand::Forge { command } => match command {
            ForgeCommand::Capabilities(repository) => {
                capabilities::capabilities(&ctx, repository.workspace).await
            }
            ForgeCommand::Api(args) => api::api(&ctx, args).await,
            ForgeCommand::Ci { command } => workflows::ci(&ctx, command).await,
        },
        RepositoryCommand::Repo {
            command: Some(command),
        } => repositories::run(&ctx, command).await,
        RepositoryCommand::Repo { command: None } => {
            unreachable!("the repository menu returns a command")
        }
    }
}
