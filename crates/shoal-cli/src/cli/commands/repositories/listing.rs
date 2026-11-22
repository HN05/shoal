//! Render registered repositories.
use anyhow::Result;

use crate::cli::{
    context::Context,
    output::{Palette, Style},
    ui,
};

pub(super) async fn list(ctx: &Context) -> Result<i32> {
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    ctx.show(&repos, |repos| {
        let palette = Palette::stdout(ctx.json);
        for repo in repos {
            let label = ui::repository_label(repo, palette);
            match repo.shared_url {
                Some(_) => outln!("{label}  {}", palette.paint(Style::Muted, "shared")),
                None => outln!("{label}"),
            }
        }
    })?;
    Ok(0)
}
