//! Confirm and remove repositories, then clean agent trust.
use anyhow::{Result, ensure};

use super::selection::chosen;
use crate::{
    cli::{
        agents,
        client::{self, request},
        context::Context,
        ui,
    },
    model::{Repository, RepositoryRemoval},
    protocol::RepositoryMethod,
    repository,
};

pub(super) async fn remove(
    ctx: &Context,
    repository: Option<String>,
    confirmed: bool,
) -> Result<i32> {
    let repository = chosen(ctx, repository).await?;
    let repositories = client::repositories(&ctx.paths).await?;
    let cleanup_repository = repository::select(&repositories, &repository)
        .await?
        .clone();
    let repository = confirm_target(ctx, repository, &cleanup_repository, confirmed)?;
    let result = ctx
        .progress(
            "Removing repository",
            request::<RepositoryRemoval>(
                &ctx.paths,
                RepositoryMethod::RemoveRepository { repository },
            ),
        )
        .await?;
    cleanup_trust(ctx, &cleanup_repository, &result).await;
    ctx.emit(
        &format!(
            "Deleted {} and {} Shoal workspaces",
            result.path.display(),
            result.workspaces_removed
        ),
        &result,
    )?;
    Ok(0)
}

fn confirm_target(
    ctx: &Context,
    repository: repository::Selector,
    target: &Repository,
    confirmed: bool,
) -> Result<repository::Selector> {
    if confirmed {
        return Ok(repository);
    }
    ensure!(
        ui::confirm(
            ctx,
            &format!(
                "Delete repository: {}\nSource: {}\nCheckout: {}\nDeletes: checkout, all Shoal workspaces and their resources\nWork:    uncommitted and unpushed changes are permanently lost",
                repository::name(target),
                target.source,
                target.path.display(),
            ),
            "--yes",
        )?,
        "repository removal canceled"
    );
    // A rename while the prompt is open must not redirect deletion.
    Ok(target.id.clone().into())
}

async fn cleanup_trust(ctx: &Context, repository: &Repository, result: &RepositoryRemoval) {
    if result.repository_id == repository.id {
        agents::cleanup_repository_trust(ctx, repository, &result.workspace_paths).await;
    }
}
