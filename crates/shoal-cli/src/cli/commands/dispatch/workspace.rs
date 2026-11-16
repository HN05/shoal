use anyhow::Result;

use crate::cli::{
    args::workspace::WorkspaceCommand,
    commands::{inbox, listing, log, rename, restore, spaces, workspaces},
    context::Context,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn run(ctx: Context, command: WorkspaceCommand) -> Result<i32> {
    match command {
        WorkspaceCommand::Inbox {
            repository,
            print: _,
        } => {
            let filter = listing::ListFilter::resolve(&ctx, repository, vec![]).await?;
            inbox::list(&ctx, &filter).await
        }
        WorkspaceCommand::List {
            chats,
            repository,
            status,
            print: _,
        } => {
            let filter = listing::ListFilter::resolve(&ctx, repository, status).await?;
            spaces::list(&ctx, &filter, chats).await
        }
        WorkspaceCommand::Status { space, item } => workspaces::status(&ctx, space, item).await,
        WorkspaceCommand::Log(args) => log::run(&ctx, args).await,
        WorkspaceCommand::Cd { workspace } => workspaces::cd(&ctx, workspace).await,
        WorkspaceCommand::Edit { workspace, args } => workspaces::edit(&ctx, workspace, args).await,
        WorkspaceCommand::Rename {
            space,
            name: Some(name),
            request: false,
        } => rename::rename(&ctx, space, name).await,
        WorkspaceCommand::Rename {
            space: None,
            name: None,
            request: false,
        } => rename::rename_picked(&ctx).await,
        // Otherwise only --request leaves out the name, and its single argument
        // takes the name's place.
        WorkspaceCommand::Rename { space, name, .. } => {
            rename::request(
                &ctx,
                name.map(|name| SpaceSelector::parse(&name))
                    .transpose()?
                    .or(space),
            )
            .await
        }
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
