//! Render registered repositories.
use anyhow::Result;

use crate::cli::{client, context::Context, output::Palette, ui};

pub(super) async fn list(ctx: &Context) -> Result<i32> {
    let repos = client::repositories(&ctx.paths).await?;
    ctx.show(&repos, |repos| {
        let palette = Palette::stdout(ctx.json);
        for repo in repos {
            outln!("{}", ui::repository_label(repo, palette));
        }
    })?;
    Ok(0)
}
