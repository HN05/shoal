//! The mode a start's agent starts in: named, else the one its PR's or
//! issue's labels call for, else on a terminal a picked one, else the default mode.
use anyhow::Result;
use shoal_protocol::launches::{LabelledMode, Launch};

use crate::{
    cli::{
        args::create::StartArgs,
        client,
        commands::{
            issues,
            start::{launch_settings, start_modes},
        },
        context::Context,
        ui::{self, ModeChoice},
    },
    config::modes::{self, ModeShapes},
    launches::{self, labels},
    model::{Repository, SpaceKind},
};

/// The mode of `launch` as far as nothing needs asking: the one it names,
/// else the [`item_mode`] of the PR or issue it names in the `found`
/// repository; otherwise one a terminal picks once the daemon has checked
/// the rest, and off one the [`launches::default_mode`].
pub(super) async fn decided(
    ctx: &Context,
    launch: &Launch,
    found: Option<&Repository>,
    modes: &ModeShapes,
) -> Result<ModeChoice> {
    if let Some(mode) = &launch.mode {
        return Ok(ModeChoice::Named(mode.clone()));
    }
    let labelled = match found {
        Some(repo) => item_mode(ctx, launch, repo, modes).await?,
        None => None,
    };
    Ok(labelled.unwrap_or_else(|| match ui::can_pick(ctx) {
        true => ModeChoice::Pick,
        false => ModeChoice::Named(launches::default_mode(launch)),
    }))
}

/// Refuse the copy and fork pickers that making the launch of `args`
/// opens where the mode they name starts a kind of space that takes
/// neither, before either picker asks anything.
pub(super) async fn ensure_located_fit(ctx: &Context, args: &StartArgs) -> Result<()> {
    let Some(mode) = args
        .mode
        .as_ref()
        .filter(|_| args.copy.is_set() || args.fork)
    else {
        return Ok(());
    };
    let launch = Launch {
        directory: ui::current_directory()?,
        ..args.launch()
    };
    let settings = launch_settings(ctx, &launch).await?;
    let modes = start_modes(ctx, &launch, &settings).await?;
    let kind = modes::get(&modes, mode)?.kind;
    super::arguments::ensure_flags_fit(args, kind, mode)
}

/// The mode `choice` names, else one picked among the `modes` the start
/// may take that start the kind of space the arguments of `launch` other
/// than the mode fit, in a repository where they name one. A setup's mode
/// is built in, which no config defines.
pub(super) fn chosen(
    ctx: &Context,
    choice: ModeChoice,
    launch: &Launch,
    modes: &ModeShapes,
) -> Result<modes::ModeName> {
    if let (ModeChoice::Named(mode), Some(_)) = (&choice, launch.setup) {
        return Ok(mode.clone());
    }
    choice.chosen(ctx, modes, labels::fit(launch)?)
}

/// The space `mode` of `launch` starts among the `modes` the start may
/// take, which says what a terminal picks for it; none for a setup's,
/// which its setup settles.
pub(super) fn configured<'a>(
    launch: &Launch,
    mode: &modes::ModeName,
    modes: &'a ModeShapes,
) -> Result<Option<&'a modes::ModeShape>> {
    match launch.setup {
        Some(_) => Ok(None),
        None => modes::get(modes, mode).map(Some),
    }
}

/// Refuse what `launch` names that the kind of space `mode` starts, among
/// the `modes` the start may take, cannot take, before a picker asks for
/// the rest; a setup's mode starts where its setup says.
pub(super) fn ensure_fits(
    launch: &Launch,
    mode: &modes::ModeName,
    modes: &ModeShapes,
) -> Result<()> {
    match configured(launch, mode, modes)? {
        Some(configured) => launches::ensure_fits(launch, &[], configured.kind, Some(mode)),
        None => Ok(()),
    }
}

/// Whether `mode`, among the `modes` the start may take, starts the agent
/// of `launch` in a workspace, where a terminal picks what the workspace
/// starts from.
pub(super) fn starts_workspace(
    launch: &Launch,
    mode: &modes::ModeName,
    modes: &ModeShapes,
) -> Result<bool> {
    let configured = configured(launch, mode, modes)?;
    Ok(configured.is_some_and(|mode| mode.kind == SpaceKind::Workspace))
}

/// The mode the labels of the PR `launch` names in `repo`, else of the
/// issue it names, call for, by [`labels::labelled_mode`] among the
/// [`labels::labelled`] `modes`; a pick on a terminal where they call for
/// several, and refused off one; none without such an item or a mode with
/// a label it carries.
async fn item_mode(
    ctx: &Context,
    launch: &Launch,
    repo: &Repository,
    modes: &ModeShapes,
) -> Result<Option<ModeChoice>> {
    let labelled = labels::labelled(launch, modes)?;
    if labelled.is_empty() {
        return Ok(None);
    }
    let Some((item, labels)) = item_labels(ctx, launch, repo).await? else {
        return Ok(None);
    };
    match labels::labelled_mode(item, labelled, &labels) {
        LabelledMode::Called(mode) => Ok(Some(ModeChoice::Named(mode))),
        LabelledMode::Unlabelled => Ok(None),
        LabelledMode::Several(_) if ui::can_pick(ctx) => Ok(Some(ModeChoice::Pick)),
        LabelledMode::Several(several) => Err(several.into()),
    }
}

/// What `launch` starts on in `repo`, the PR it names, else the issue, as
/// refusals name it, with its labels; none where it names neither.
async fn item_labels(
    ctx: &Context,
    launch: &Launch,
    repo: &Repository,
) -> Result<Option<(String, Vec<String>)>> {
    if let Some(pull) = launches::named_pull(launch) {
        let context = issues::read_context(ctx, repo).await?;
        let pull = client::pull_request(&ctx.paths, context, pull).await?;
        return Ok(Some((format!("PR {}", pull.url), pull.labels)));
    }
    let Some(input) = launches::named_issue(launch)? else {
        return Ok(None);
    };
    let context = issues::read_context(ctx, repo).await?;
    let issue = issues::load(ctx, context, input.clone()).await?;
    Ok(Some((format!("issue {input}"), issue.labels)))
}

#[cfg(test)]
mod tests;
