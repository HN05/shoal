use anyhow::Result;

use crate::cli::{
    args::create::CreateCommand,
    commands::{contribute, links, split, workspaces},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: CreateCommand) -> Result<i32> {
    match command {
        CreateCommand::Add {
            here,
            focus,
            path,
            repository,
            repository_override,
            branch,
            existing,
            issue,
            base,
            git_profile,
            agent,
            now,
            after,
            model,
            args,
        } => {
            links::add(
                &ctx,
                links::AddInput {
                    positional: repository,
                    repository: repository_override,
                    issue,
                },
                workspaces::Creation {
                    path,
                    branch,
                    existing,
                    base,
                    git_profile,
                    pr: None,
                },
                agent,
                args,
                workspaces::AddOptions {
                    here,
                    focus,
                    now,
                    after,
                    model: model.into(),
                },
            )
            .await
        }
        CreateCommand::Contribute(args) => contribute::contribute(&ctx, args).await,
        CreateCommand::Split {
            branch,
            issue,
            base,
            workspace,
            agent,
            fork,
            model,
            args,
        } => {
            let split = split::Split {
                branch,
                issue,
                base,
                agent,
                fork,
                model: model.into(),
                args,
            };
            split::run(&ctx, workspace, split).await
        }
    }
}
