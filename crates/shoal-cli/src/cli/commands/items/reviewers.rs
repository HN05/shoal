//! `shoal pr reviewers`.
use anyhow::Result;

use super::item_target;
use crate::{
    cli::{args::changes::ItemSelector, client, context::Context},
    forge::items::review::reviewers::Reviewer,
};

pub(super) async fn reviewers(ctx: &Context, selector: &ItemSelector) -> Result<i32> {
    let reading = {
        let item = selector.item.clone();
        super::in_context(move |target| client::reviewers(target, item.clone()))
    };
    let target = item_target(ctx, selector, &reading).await?;
    let read = client::reviewers(target, selector.item.clone()).send(&ctx.paths);
    let reviewers = ctx.progress("Reading reviewers", read).await?;
    ctx.show(&reviewers, |reviewers| render(reviewers))?;
    Ok(0)
}

fn render(reviewers: &[Reviewer]) {
    if reviewers.is_empty() {
        outln!("No reviewers");
        return;
    }
    let width = reviewers
        .iter()
        .map(|reviewer| reviewer.name.len())
        .max()
        .unwrap_or(0);
    for reviewer in reviewers {
        let team = if reviewer.team { " (team)" } else { "" };
        let line = format!("{:width$}  {}{team}", reviewer.name, reviewer.display_name);
        outln!("{}", line.trim_end());
    }
}
