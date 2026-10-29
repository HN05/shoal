use anyhow::Result;

use crate::cli::{
    args::items::ItemsCommand,
    commands::{links, view},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: ItemsCommand) -> Result<i32> {
    match command {
        ItemsCommand::Link { items } => links::link(&ctx, items).await,
        ItemsCommand::Unlink { items } => links::unlink(&ctx, items).await,
        ItemsCommand::View {
            items, web: true, ..
        } => view::open(&ctx, items).await,
        ItemsCommand::View {
            items,
            no_comments,
            check,
            web: false,
        } => view::view(&ctx, items, no_comments, check).await,
        ItemsCommand::Watch {
            items,
            timeout,
            resume,
        } => links::watch(&ctx, items, timeout, resume).await,
        ItemsCommand::Ready {
            items,
            undo,
            draft,
            no_draft,
        } => {
            if undo {
                links::unready(&ctx, items, (draft || no_draft).then_some(draft)).await
            } else {
                links::ready(&ctx, items).await
            }
        }
    }
}
