use anyhow::Result;

use crate::cli::{
    args::create::{ConvertCommand, CreateCommand},
    commands::{contribute, convert, split, start},
    context::Context,
};
use crate::protocol::CreationOptions;

pub(in crate::cli::commands) async fn run(ctx: Context, command: CreateCommand) -> Result<i32> {
    match command {
        CreateCommand::Start(args) => start::run(&ctx, *args).await,
        CreateCommand::Contribute(args) => contribute::contribute(&ctx, args).await,
        CreateCommand::Split {
            branch,
            no_branch,
            workspace,
            copy,
            disposable,
        } => {
            let options = CreationOptions {
                skip_setup: branch.skip_setup,
                disposable,
            };
            if no_branch {
                return split::at_commit(&ctx, workspace, branch.base, copy, options).await;
            }
            let split = split::Split {
                options,
                branch: branch.branch,
                issue: branch.issue,
                base: branch.base,
                copy,
            };
            split::run(&ctx, workspace, split).await
        }
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
