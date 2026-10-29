use anyhow::Result;

use crate::{
    cli::{
        args::changes::{BranchCommand, ChangesCommand},
        commands::{base, items, review, workspaces},
        context::Context,
    },
    forge::items::link::ItemKind,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: ChangesCommand) -> Result<i32> {
    match command {
        ChangesCommand::Pr { command } => items::pr(&ctx, command).await,
        ChangesCommand::Issue { command } => items::issue(&ctx, command).await,
        ChangesCommand::Branch { command } => branch(&ctx, command).await,
        ChangesCommand::Diff { workspace } => workspaces::diff(&ctx, workspace).await,
        ChangesCommand::Conflicts { target, workspace } => {
            workspaces::conflicts(&ctx, workspace, target).await
        }
        ChangesCommand::Review {
            workspace,
            pr,
            issue,
            repository,
            post,
            no_post,
            manual,
            agent,
            args,
        } => {
            let item = pr
                .map(|pr| (ItemKind::Pr, pr))
                .or(issue.map(|issue| (ItemKind::Issue, issue)));
            let post = (post || no_post).then_some(post);
            let target = review::Target::new(workspace, item, repository, post)?;
            let reviewer = review::Reviewer::new(manual, agent);
            review::start(&ctx, target, reviewer, args).await
        }
        ChangesCommand::Land {
            workspace,
            push,
            no_push,
        } => workspaces::land(&ctx, workspace, (push || no_push).then_some(push)).await,
    }
}

async fn branch(ctx: &Context, command: BranchCommand) -> Result<i32> {
    match command {
        BranchCommand::Base { command, workspace } => base::run(ctx, command, workspace).await,
    }
}
