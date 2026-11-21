use anyhow::Result;

use crate::cli::{
    args::workspace::WorkspaceCommand,
    commands::{adopt, cd, edit, refresh, restore, setup},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: WorkspaceCommand) -> Result<i32> {
    match command {
        WorkspaceCommand::Cd { workspace } => cd::cd(&ctx, workspace).await,
        WorkspaceCommand::Edit { workspace, args } => edit::edit(&ctx, workspace, args).await,
        WorkspaceCommand::Adopt {
            path,
            repository,
            copy,
        } => adopt::adopt(&ctx, path, repository, copy).await,
        WorkspaceCommand::Setup {
            workspace,
            skip: false,
        } => setup::setup(&ctx, workspace).await,
        WorkspaceCommand::Setup {
            workspace,
            skip: true,
        } => setup::skip_setup(&ctx, workspace).await,
        WorkspaceCommand::Refresh { workspace } => refresh::refresh(&ctx, workspace).await,
        WorkspaceCommand::Checkpoint { workspace, name } => {
            restore::checkpoint(&ctx, workspace, name).await
        }
    }
}
