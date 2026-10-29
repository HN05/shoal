//! `shoal pr reviewers`: who a PR can ask for a review, named as
//! `shoal pr edit --add-reviewer` takes them.
use anyhow::{Result, bail};

use super::select_prs;
use crate::{
    cli::{args::changes::ItemSelector, context::Context},
    forge::{ForgeRepo, review::reviewers::Reviewer},
};

pub(super) async fn reviewers(ctx: &Context, selector: &ItemSelector) -> Result<i32> {
    let selected = select_prs(ctx, selector).await?;
    let [item] = selected.items.as_slice() else {
        bail!("several PRs are linked; pass a number or URL");
    };
    let forge = ForgeRepo::resolve_pull_url(&item.url, &ctx.forge_hosts()?).await?;
    let (number, _) = forge.pull(&item.url)?;
    let reviewers = ctx
        .progress(
            "Reading reviewers",
            forge.reviewers(&ctx.forge_account()?, number),
        )
        .await?;
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
