//! Confirm and remove repositories.
use anyhow::{Result, ensure};

use super::selection::chosen;
use crate::{
    cli::{context::Context, ui},
    model::Repository,
    repository,
};

pub(super) async fn remove(
    ctx: &Context,
    repository: Option<String>,
    confirmed: bool,
) -> Result<i32> {
    let repository = chosen(ctx, repository).await?;
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let target = repository::select(&repositories, &repository).await?;
    let repository = confirm_target(ctx, repository, target, confirmed)?;
    let result = ctx
        .progress(
            "Removing repository",
            crate::client::repositories::remove(&ctx.paths, repository),
        )
        .await?;
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
                target.display_name(),
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
