use anyhow::{Result, ensure};

use crate::{
    cli::{
        args::items::ItemsCommand,
        commands::{ci, links, view},
        context::Context,
    },
    forge::ci::run_view::JobRef,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: ItemsCommand) -> Result<i32> {
    match command {
        ItemsCommand::Link { items } => links::link(&ctx, items).await,
        ItemsCommand::Unlink { items } => links::unlink(&ctx, items).await,
        ItemsCommand::View {
            items,
            no_comments,
            check,
            job,
            job_id,
            web,
        } => {
            let job = job.map(JobRef::Name).or(job_id.map(JobRef::Id));
            if items.selects_ci() {
                ensure!(
                    check.is_none(),
                    "--check reads a PR's check; use --job for a run's"
                );
                return ci::view(&ctx, items, job, web).await;
            }
            ensure!(
                job.is_none(),
                "--job and --job-id read a CI run's job: view ci RUN --job NAME"
            );
            if web {
                view::open(&ctx, items).await
            } else {
                view::view(&ctx, items, no_comments, check).await
            }
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
