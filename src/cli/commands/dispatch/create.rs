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
            creation,
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
                    options: creation.into(),
                    pull_head: None,
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
            no_branch,
            issue,
            base,
            workspace,
            agent,
            fork,
            creation,
            model,
            args,
        } => {
            if no_branch {
                return split::at_commit(&ctx, workspace, base, creation.into()).await;
            }
            let split = split::Split {
                options: creation.into(),
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
