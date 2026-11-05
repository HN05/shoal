use anyhow::Result;

use crate::cli::{
    args::create::CreateCommand,
    commands::{contribute, split, start},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: CreateCommand) -> Result<i32> {
    match command {
        CreateCommand::Start(args) => start::run(&ctx, *args).await,
        CreateCommand::Contribute(args) => contribute::contribute(&ctx, args).await,
        CreateCommand::Split {
            branch,
            no_branch,
            issue,
            base,
            workspace,
            creation,
        } => {
            if no_branch {
                return split::at_commit(&ctx, workspace, base, creation.into()).await;
            }
            let split = split::Split {
                options: creation.into(),
                branch,
                issue,
                base,
            };
            split::run(&ctx, workspace, split).await
        }
    }
}
