//! Render the registered repositories: this machine's, then the other
//! joined machines'.
use anyhow::Result;
use shoal_client::output::repositories::ListedRepository as Listed;

use super::elsewhere;
use crate::cli::{
    context::Context,
    output::{Palette, Style},
    ui,
};

pub(super) async fn list(ctx: &Context) -> Result<i32> {
    let here = crate::client::repositories::list(&ctx.paths).await?;
    let (others, _) = elsewhere::registered(ctx).await;
    let listed: Vec<Listed> = (here.into_iter().map(Listed::Here))
        .chain(others.into_iter().map(Listed::Elsewhere))
        .collect();
    ctx.show(&listed, |listed| {
        let palette = Palette::stdout(ctx.json);
        for listed in listed {
            outln!("{}", row(listed, palette));
        }
    })?;
    Ok(0)
}

/// The line that shows `listed`.
fn row(listed: &Listed, palette: Palette) -> String {
    let (label, shared) = match listed {
        Listed::Here(repo) => (
            ui::repository_label(repo, palette),
            repo.shared_url.is_some(),
        ),
        Listed::Elsewhere(other) => (
            format!(
                "{}  {}  {}",
                palette.paint(Style::Heading, other.name()),
                palette.paint(Style::Muted, other.source()),
                palette.paint(Style::Muted, format!("on {}", other.machine)),
            ),
            other.registered.shared,
        ),
    };
    match shared {
        true => format!("{label}  {}", palette.paint(Style::Muted, "shared")),
        false => label,
    }
}
