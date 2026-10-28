use anyhow::Result;

use crate::cli::{
    args::workspace::WorkspaceCommand,
    commands::{chat, inbox, listing, work, workspaces},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: WorkspaceCommand) -> Result<i32> {
    match command {
        WorkspaceCommand::Inbox {
            repository,
            print: _,
        } => {
            let filter = listing::ListFilter::resolve(&ctx, repository, vec![]).await?;
            inbox::list(&ctx, &filter).await
        }
        WorkspaceCommand::Work {
            command: Some(command),
            ..
        } => work::run(&ctx, command).await,
        WorkspaceCommand::Work {
            command: None,
            chats,
            repository,
            status,
            print: _,
        } => {
            let filter = listing::ListFilter::resolve(&ctx, repository, status).await?;
            if chats {
                return chat::list(&ctx, &filter).await;
            }
            workspaces::list(&ctx, &filter).await
        }
        WorkspaceCommand::Status { workspace, item } => {
            workspaces::status(&ctx, workspace, item).await
        }
        WorkspaceCommand::Cd { workspace } => workspaces::cd(&ctx, workspace).await,
        WorkspaceCommand::Edit { workspace, args } => workspaces::edit(&ctx, workspace, args).await,
    }
}
