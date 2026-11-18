use anyhow::Result;

use crate::cli::{
    args::create::{ConvertCommand, CreateCommand},
    commands::{contribute, convert, create, start},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: CreateCommand) -> Result<i32> {
    match command {
        CreateCommand::Start(args) => start::run(&ctx, *args).await,
        CreateCommand::Contribute(args) => contribute::contribute(&ctx, args).await,
        CreateCommand::Create(args) => create::run(&ctx, *args).await,
        CreateCommand::Convert { command } => match command {
            ConvertCommand::Workspace {
                branch,
                mode,
                space,
            } => convert::to_workspace(&ctx, space, branch, mode).await,
            ConvertCommand::Chat { mode, space } => convert::to_chat(&ctx, space, mode).await,
        },
    }
}
