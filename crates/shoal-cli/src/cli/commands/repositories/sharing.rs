//! `shoal repo share`.
use anyhow::Result;

use super::selection::chosen;
use crate::cli::{context::Context, output::Palette, ui};

pub(super) async fn share(ctx: &Context, repository: Option<String>, shared: bool) -> Result<i32> {
    let repository = chosen(ctx, repository).await?;
    let repo = crate::client::repositories::share(&ctx.paths, repository, shared).await?;
    let done = match shared {
        true => "Shared",
        false => "Stopped sharing",
    };
    let label = ui::repository_label(&repo, Palette::stdout(ctx.json));
    ctx.emit(&format!("{done} {label}"), &repo)?;
    Ok(0)
}
