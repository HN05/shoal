use anyhow::Result;

use crate::cli::{
    args::space::SpaceCommand,
    commands::{inbox, listing, log, spaces},
    context::Context,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn run(ctx: Context, command: SpaceCommand) -> Result<i32> {
    match command {
        SpaceCommand::Open { space, app, args } => spaces::open(&ctx, space, app, args).await,
        SpaceCommand::Inbox {
            repository,
            print: _,
        } => {
            let filter = listing::ListFilter::resolve(&ctx, repository, vec![]).await?;
            inbox::list(&ctx, &filter).await
        }
        SpaceCommand::List {
            chats,
            repository,
            status,
            print: _,
        } => {
            let filter = listing::ListFilter::resolve(&ctx, repository, status).await?;
            spaces::list(&ctx, &filter, chats).await
        }
        SpaceCommand::Status { space, item } => spaces::status(&ctx, space, item).await,
        SpaceCommand::Log(args) => log::run(&ctx, args).await,
        SpaceCommand::Rename {
            space,
            name: Some(name),
            request: false,
        } => spaces::rename(&ctx, space, name).await,
        SpaceCommand::Rename {
            space: None,
            name: None,
            request: false,
        } => spaces::rename_picked(&ctx).await,
        // Otherwise only --request leaves out the name, and its single argument
        // takes the name's place.
        SpaceCommand::Rename { space, name, .. } => {
            spaces::request_name(
                &ctx,
                name.map(|name| SpaceSelector::parse(&name))
                    .transpose()?
                    .or(space),
            )
            .await
        }
    }
}
