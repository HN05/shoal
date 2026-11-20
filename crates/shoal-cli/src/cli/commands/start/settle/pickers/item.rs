//! The issue or PR a start asks for without naming it, picked on a
//! terminal among its repository's open ones, only those carrying the
//! label of the mode it names where that mode has one. The picked item's
//! labels then decide the mode a start that names none takes.
use anyhow::{Context as _, Result};
use shoal_protocol::launches::{ItemChoice, Launch};

use crate::{
    cli::{client, commands::issues, context::Context, ui},
    config::Effective,
    forge::items::{link::ItemKind, list::Search, reference::ItemInput},
    model::Repository,
};

/// The issue and PR `launch` asks for without naming them, picked on a
/// terminal in its `found` repository, else one a terminal picks, which
/// the start then goes in under its own settings; off a terminal, refused.
/// Returns the repository and settings the start goes on with.
pub(in crate::cli::commands::start::settle) async fn pick_items(
    ctx: &Context,
    launch: &mut Launch,
    (found, settings): (Option<Repository>, Effective),
) -> Result<(Option<Repository>, Effective)> {
    let picks = |choice: &Option<ItemChoice>| *choice == Some(ItemChoice::Pick);
    if !picks(&launch.issue) && !picks(&launch.pr) {
        return Ok((found, settings));
    }
    let required = super::super::repository::required(ctx, launch, (found, settings));
    let (found, settings) = required.await?;
    let label = mode_label(launch, &settings);
    for (choice, kind) in [
        (&mut launch.issue, ItemKind::Issue),
        (&mut launch.pr, ItemKind::Pr),
    ] {
        if picks(choice) {
            let picked = picked(ctx, found.as_ref(), kind, label.as_deref()).await?;
            *choice = Some(ItemChoice::Named(picked));
        }
    }
    Ok((found, settings))
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
