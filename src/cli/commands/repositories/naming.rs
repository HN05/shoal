//! Choose and save repository display names.
use anyhow::Result;

use super::selection::chosen;
use crate::{
    cli::{
        client::{self, request},
        context::Context,
        output::Palette,
        ui,
    },
    model::Repository,
    protocol::RepositoryMethod,
    repository,
};

pub(super) async fn rename(
    ctx: &Context,
    repository: Option<String>,
    name: Option<String>,
) -> Result<i32> {
    let repository = chosen(ctx, repository).await?;
    let name = new_name(ctx, &repository, name).await?;
    let repo = request::<Repository>(
        &ctx.paths,
        RepositoryMethod::RenameRepository { repository, name },
    )
    .await?;
    ctx.emit(
        &format!(
            "Renamed {}",
            ui::repository_label(&repo, Palette::stdout(ctx.json))
        ),
        &repo,
    )?;
    Ok(0)
}

async fn new_name(
    ctx: &Context,
    repository: &repository::Selector,
    name: Option<String>,
) -> Result<String> {
    match name {
        Some(name) => Ok(name),
        None => {
            // Refuse an unknown repository before asking for its name.
            repository::select(&client::repositories(&ctx.paths).await?, repository).await?;
            ui::require_argument(ctx, "new name", "name")?;
            ui::input(ctx, "New name")
        }
    }
}
