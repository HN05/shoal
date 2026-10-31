use anyhow::{Result, ensure};

use crate::cli::{
    args::items::{ItemsCommand, ViewArgs},
    commands::{links, review, view},
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: ItemsCommand) -> Result<i32> {
    match command {
        ItemsCommand::Link { items } => links::link(&ctx, items).await,
        ItemsCommand::Unlink { items } => links::unlink(&ctx, items).await,
        ItemsCommand::View(args) if args.items.selects_review() => view_review(&ctx, args).await,
        ItemsCommand::View(args) => view::show(&ctx, args).await,
        ItemsCommand::Watch { items, timeout, .. } if items.selects_review() => {
            ensure!(
                timeout.is_none(),
                "watch review waits until the review ends; --timeout bounds PR and issue watches"
            );
            review::show::watch(&ctx, items.review_id()?, items.workspace).await
        }
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

/// `view review [ID]`, which takes none of the item options.
async fn view_review(ctx: &Context, args: ViewArgs) -> Result<i32> {
    ensure!(
        !args.no_comments
            && args.check.is_none()
            && args.job.is_none()
            && args.job_id.is_none()
            && !args.web
            && args.repo.name.is_none(),
        "view review takes a review ID and --workspace only"
    );
    review::show::view(ctx, args.items.review_id()?, args.items.workspace).await
}
