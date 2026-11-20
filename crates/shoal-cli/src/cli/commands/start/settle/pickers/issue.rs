//! The issue a start in plan mode settles the plan of when it asks for one
//! without naming it, picked among those labelled as needing a plan. The
//! plan goes to the issue's body, which is all an implementing agent
//! receives.
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::launches::{ItemChoice, Launch};

use crate::{
    cli::{client, commands::issues, context::Context, ui},
    config::{self, Effective},
    forge::items::{link::ItemKind, list::Search, reference::ItemInput},
    model::Repository,
};

/// The issue a start in plan mode that asks for one without naming it
/// plans, picked on a terminal among the open issues of its `repository`
/// labelled with the plan mode's label under its `settings`; a launch that
/// names one keeps it.
pub(in crate::cli::commands::start::settle) async fn pick_plan_issue(
    ctx: &Context,
    launch: &mut Launch,
    (repository, settings): (Option<&Repository>, &Effective),
) -> Result<()> {
    if !matches!(launch.issue, Some(ItemChoice::Pick)) || !ctx.interactive() {
        return Ok(());
    }
    let repo = repository.context("name the repository whose issue the plan settles")?;
    let mode = (launch.mode.clone()).unwrap_or(config::modes::ModeName::DEFAULT);
    let issue = picked(ctx, repo, settings, &mode).await?;
    launch.issue = Some(ItemChoice::Named(issue));
    Ok(())
}

/// In `mode` plan, the number of an open issue of `repo` labelled with the
/// plan mode's label under `settings`, picked; other modes start on a
/// named issue only.
async fn picked(
    ctx: &Context,
    repo: &Repository,
    settings: &Effective,
    mode: &config::modes::ModeName,
) -> Result<ItemInput> {
    ensure!(
        *mode == config::modes::ModeName::PLAN,
        "--issue needs a number or URL outside plan mode"
    );
    let label = (settings.modes.get(mode.as_str()))
        .and_then(|mode| mode.label.as_deref())
        .context(
            "set [modes.plan] label to choose among the issues that need a plan, or name an issue",
        )?;
    let context = issues::read_context(ctx, repo).await?;
    let search = Search::labelled(label);
    let issues = client::list_items(&ctx.paths, context.into(), ItemKind::Issue, search).await?;
    ui::require_argument(ctx, "issue", "issue")?;
    let number = ui::pick_item(ctx, "Issue> ", &format!("issues labelled {label}"), issues)?;
    Ok(ItemInput::Number(number))
}
