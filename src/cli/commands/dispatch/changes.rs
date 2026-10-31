use anyhow::Result;

use crate::cli::{
    args::changes::{BranchCommand, ChangesCommand},
    commands::{base, ci, items, review, workspaces},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: ChangesCommand) -> Result<i32> {
    match command {
        ChangesCommand::Pr { command } => items::pr(&ctx, command).await,
        ChangesCommand::Issue { command } => items::issue(&ctx, command).await,
        ChangesCommand::Ci { command } => ci::run(&ctx, command).await,
        ChangesCommand::Branch { command } => branch(&ctx, command).await,
        ChangesCommand::Diff { workspace } => workspaces::diff(&ctx, workspace).await,
        ChangesCommand::Conflicts { target, workspace } => {
            workspaces::conflicts(&ctx, workspace, target).await
        }
        ChangesCommand::Review { command } => review::run(&ctx, command).await,
        ChangesCommand::Land {
            workspace,
            push,
            no_push,
        } => workspaces::land(&ctx, workspace, (push || no_push).then_some(push)).await,
    }
}

async fn branch(ctx: &Context, command: BranchCommand) -> Result<i32> {
    match command {
        BranchCommand::Rename { workspace, branch } => {
            workspaces::rename(ctx, workspace, branch).await
        }
        BranchCommand::Base { command, workspace } => base::run(ctx, command, workspace).await,
    }
}
