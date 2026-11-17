//! `shoal pr reviewers`: who a PR can ask for a review, named as
//! `shoal pr edit --add-reviewer` takes them.
use anyhow::Result;

use super::item_target;
use crate::{
    cli::{args::changes::ItemSelector, client, context::Context},
    forge::review::reviewers::Reviewer,
};

pub(super) async fn reviewers(ctx: &Context, selector: &ItemSelector) -> Result<i32> {
    let target = item_target(ctx, selector).await?;
    let read = client::reviewers(&ctx.paths, target, selector.item.clone());
    let reviewers = ctx.progress("Reading reviewers", read).await?;
    ctx.show(&reviewers, |reviewers| render(reviewers))?;
    Ok(0)
}

/// One line per reviewer: the name to pass, the name the forge shows, and
/// whether it is a team.
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
