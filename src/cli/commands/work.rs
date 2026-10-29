//! Workspace maintenance commands beneath `shoal work`.
use anyhow::Result;

use crate::cli::{args::workspace::WorkCommand, context::Context};

use super::{restore, workspaces};

pub(super) async fn run(ctx: &Context, command: WorkCommand) -> Result<i32> {
    match command {
        WorkCommand::Env { workspace, revoke } => {
            workspaces::environment(ctx, workspace, revoke).await
        }
        WorkCommand::Adopt {
            path,
            repository,
            copy,
        } => workspaces::adopt(ctx, path, repository, copy).await,
        WorkCommand::Title { workspace, title } => workspaces::title(ctx, workspace, title).await,
        WorkCommand::Checkpoint { workspace, name } => {
            restore::checkpoint(ctx, workspace, name).await
        }
        WorkCommand::Restore {
            workspace,
            checkpoint,
        } => restore::run(ctx, workspace, checkpoint).await,
        WorkCommand::Setup {
            workspace,
            skip: false,
        } => workspaces::setup(ctx, workspace).await,
        WorkCommand::Setup {
            workspace,
            skip: true,
        } => workspaces::skip_setup(ctx, workspace).await,
    }
}
