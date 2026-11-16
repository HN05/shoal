use anyhow::Result;

use crate::cli::{
    args::workspace::WorkspaceCommand,
    commands::{restore, workspaces},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: WorkspaceCommand) -> Result<i32> {
    match command {
        WorkspaceCommand::Cd { workspace } => workspaces::cd(&ctx, workspace).await,
        WorkspaceCommand::Edit { workspace, args } => workspaces::edit(&ctx, workspace, args).await,
        WorkspaceCommand::Adopt {
            path,
            repository,
            copy,
        } => workspaces::adopt(&ctx, path, repository, copy).await,
        WorkspaceCommand::Setup {
            workspace,
            skip: false,
        } => workspaces::setup(&ctx, workspace).await,
        WorkspaceCommand::Setup {
            workspace,
            skip: true,
        } => workspaces::skip_setup(&ctx, workspace).await,
        WorkspaceCommand::Refresh { workspace } => workspaces::refresh(&ctx, workspace).await,
        WorkspaceCommand::Checkpoint { workspace, name } => {
            restore::checkpoint(&ctx, workspace, name).await
        }
    }
}
