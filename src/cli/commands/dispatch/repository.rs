use anyhow::Result;

use crate::cli::{
    args::repository::{ForgeCommand, RepositoryCommand},
    commands::{api, capabilities, reactions, repositories},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: RepositoryCommand) -> Result<i32> {
    match command {
        RepositoryCommand::Forge { command } => match command {
            ForgeCommand::Capabilities(repository) => {
                capabilities::capabilities(&ctx, repository).await
            }
            ForgeCommand::Reactions(repository) => reactions::reactions(&ctx, repository).await,
            ForgeCommand::Api(args) => api::api(&ctx, args).await,
        },
        RepositoryCommand::Repo {
            command: Some(command),
        } => repositories::run(&ctx, command).await,
        RepositoryCommand::Repo { command: None } => {
            unreachable!("the repository menu returns a command")
        }
    }
}
