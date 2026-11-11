//! Refresh a repository without moving workspace branches.
use anyhow::Result;

use crate::{
    cli::{context::Context, ui},
    model::SyncedRepository,
    repository,
};

pub(super) async fn sync(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let repository = sync_target(ctx, repository).await?;
    let synced = crate::client::repositories::sync(&ctx.paths, repository).await?;
    ctx.show(&synced, render)?;
    Ok(0)
}

async fn sync_target(ctx: &Context, repository: Option<String>) -> Result<repository::Selector> {
    Ok(match repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => super::super::issues::current_repository(
            ctx,
            crate::client::repositories::list(&ctx.paths).await?,
            "pass a repository",
        )
        .await?
        .into(),
    })
}

fn render(synced: &SyncedRepository) {
    if let Some(remote) = &synced.remote {
        outln!("Fetched {remote}");
    }
    let refresh = &synced.default_branch;
    match (&refresh.skipped, refresh.updated) {
        (Some(skipped), _) => outln!("{skipped}"),
        (None, true) => outln!(
            "Updated {} from its upstream ({}..{})",
            refresh.branch,
            refresh.previous_commit,
            refresh.commit
        ),
        (None, false) => outln!("{} is up to date with its upstream", refresh.branch),
    }
}
