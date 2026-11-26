//! `shoal queue`.
mod attach;
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
        Some(QueueCommand::Start { entry, attach }) => changes::start(ctx, entry, attach).await,
        Some(QueueCommand::Cancel { entry }) => changes::cancel(ctx, entry).await,
    }
}
