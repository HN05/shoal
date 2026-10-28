//! `shoal queue`: the agent queue of every repository, and starting, moving
//! and cancelling its entries, named or picked.
mod changes;
mod listing;
mod menu;
mod selection;

use anyhow::Result;

use crate::cli::{args::sessions::QueueCommand, context::Context};

pub(super) async fn run(
    ctx: &Context,
    command: Option<QueueCommand>,
    repository: Option<String>,
    print: bool,
) -> Result<i32> {
    match command {
        None if repository.is_none() && !print && ctx.interactive_output() => {
            menu::choose(ctx).await
        }
        None => listing::list(ctx, repository).await,
        Some(QueueCommand::Start { entry }) => changes::start(ctx, entry).await,
        Some(QueueCommand::Move { entry, position }) => {
            changes::move_entry(ctx, entry, position).await
        }
        Some(QueueCommand::Cancel { entry }) => changes::cancel(ctx, entry).await,
    }
}
