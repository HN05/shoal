//! The issue a start in plan mode settles the plan of when it asks for one
//! without naming it, picked among those labelled as needing a plan. The
//! plan goes to the issue's body, which is all an implementing agent
//! receives.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{client, commands::issues, context::Context, ui},
    config,
    forge::items::{link::ItemKind, list::Search, reference::ItemInput},
    model::Repository,
};

/// In `mode` plan, the number of an open issue of `repo` labelled `[issue]
/// plan_label`, picked; other modes start on a named issue only.
pub(super) async fn picked(
    ctx: &Context,
    repo: &Repository,
    labels: &config::Issue,
    mode: &config::modes::ModeName,
) -> Result<ItemInput> {
    ensure!(
        *mode == config::modes::ModeName::PLAN,
        "--issue needs a number or URL outside plan mode"
    );
    let label = labels.plan_label.as_deref().context(
        "set [issue] plan_label to choose among the issues that need a plan, or name an issue",
    )?;
    let context = issues::read_context(ctx, repo).await?;
    let search = Search::labelled(label);
    let issues = client::list_items(&ctx.paths, context.into(), ItemKind::Issue, search).await?;
    ui::require_argument(ctx, "issue", "issue")?;
    let number = ui::pick_item(ctx, "Issue> ", &format!("issues labelled {label}"), issues)?;
    Ok(ItemInput::Number(number))
}
