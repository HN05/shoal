//! The issue, PR or CI run a start asks for without naming it, picked on a
//! terminal: an issue or PR among its repository's open ones, only those
//! carrying the label of the mode it names where that mode has one, whose
//! labels then decide the mode a start that names none takes, and a run
//! among those that fail now.
use anyhow::{Context as _, Result};
use shoal_protocol::launches::{ItemChoice, Launch};

use crate::{
    cli::{client, commands::issues, context::Context, ui},
    config::Effective,
    forge::{
        ci::run_view::{RunFilter, still_failing},
        items::{
            link::{ItemKind, LinkKind},
            list::Search,
            reference::{ItemInput, ItemRef},
        },
    },
    model::Repository,
    protocol::RunsOf,
};

/// Whether `launch` asks for an issue, PR or CI run without naming it.
pub(in crate::cli::commands::start::settle) fn picks_items(launch: &Launch) -> bool {
    [&launch.issue, &launch.pr, &launch.ci].contains(&&Some(ItemChoice::Pick))
}

/// The issue, PR and CI run `launch` asks for without naming them, picked
/// on a terminal in `repository` under its `settings`; off a terminal,
/// refused.
pub(in crate::cli::commands::start::settle) async fn pick_items(
    ctx: &Context,
    launch: &mut Launch,
    (repository, settings): (Option<&Repository>, &Effective),
) -> Result<()> {
    let label = mode_label(launch, settings);
    for (choice, kind) in [
        (&mut launch.issue, ItemKind::Issue),
        (&mut launch.pr, ItemKind::Pr),
    ] {
        if *choice == Some(ItemChoice::Pick) {
            let picked = picked(ctx, repository, kind, label.as_deref()).await?;
            *choice = Some(ItemChoice::Named(picked));
        }
    }
    if launch.ci == Some(ItemChoice::Pick) {
        let run = picked_run(ctx, repository).await?;
        launch.ci = Some(ItemChoice::Named(ItemInput::Url(run)));
    }
    Ok(())
}

/// The label of the mode `launch` names under `settings`, if any.
fn mode_label(launch: &Launch, settings: &Effective) -> Option<String> {
    let mode = settings.modes.get(launch.mode.as_ref()?.as_str())?;
    mode.label.clone()
}

/// The number of an open item of `kind` in `repo`, carrying `label` where
/// there is one, picked; refused off a terminal.
async fn picked(
    ctx: &Context,
    repo: Option<&Repository>,
    kind: ItemKind,
    label: Option<&str>,
) -> Result<ItemInput> {
    let (flag, prompt, noun) = match kind {
        ItemKind::Pr => ("pr", "Pull request> ", "pull requests"),
        _ => ("issue", "Issue> ", "issues"),
    };
    ui::require_argument(ctx, flag, flag)?;
    let repo = repo.with_context(|| format!("name the repository whose {noun} to pick from"))?;
    let context = issues::read_context(ctx, repo).await?;
    let search = label.map_or_else(Search::picker, Search::labelled);
    let items = client::list_items(&ctx.paths, context.into(), kind, search).await?;
    let described = match label {
        Some(label) => format!("{noun} labelled {label}"),
        None => noun.to_owned(),
    };
    let number = ui::pick_item(ctx, prompt, &described, items)?;
    Ok(ItemInput::Number(number))
}

/// The newest run of each workflow and ref in `repo` that fails now,
/// picked; refused off a terminal.
async fn picked_run(ctx: &Context, repo: Option<&Repository>) -> Result<ItemRef> {
    ui::require_argument(ctx, "ci", "ci")?;
    let repo = repo.context("name the repository whose CI runs to pick from")?;
    let context = issues::read_context(ctx, repo).await?;
    let filter = RunFilter {
        branch: None,
        workflow: None,
        commit: None,
        limit: shoal_forge::ci::run_view::MAX_RUNS,
    };
    let runs = crate::client::ci::runs(context.into(), RunsOf::EveryBranch, filter);
    let runs = still_failing(runs.send(&ctx.paths).await?);
    let url = ui::pick_run(ctx, "CI run> ", "failing CI runs", runs)?;
    ItemRef::parse_kind(LinkKind::Ci, &url)
}
