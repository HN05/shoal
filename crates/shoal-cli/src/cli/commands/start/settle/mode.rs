//! The mode a start's agent starts in: named, else the one its issue's
//! labels call for, else on a terminal a picked one, else the default mode.
use anyhow::Result;
use shoal_protocol::launches::{ItemChoice, Launch};

use crate::{
    cli::{
        args::create::StartArgs,
        commands::{
            issues,
            start::{launch_settings, start_modes},
        },
        context::Context,
        ui::{self, ModeChoice, ModeFit},
    },
    config::{
        Effective,
        modes::{self, ModeShapes},
    },
    launches,
    model::{Repository, SpaceKind},
};

/// The mode of `launch` as far as nothing needs asking: the one it names,
/// on a terminal the plan mode for an issue to pick, which no other mode
/// takes, else the [`item_mode`] of the issue it names in the `found`
/// repository; otherwise one a terminal picks once the daemon has checked
/// the rest, and off one the [`launches::default_mode`].
pub(super) async fn decided(
    ctx: &Context,
    launch: &Launch,
    found: Option<&Repository>,
    (settings, modes): (&Effective, &ModeShapes),
) -> Result<ModeChoice> {
    if let Some(mode) = &launch.mode {
        return Ok(ModeChoice::Named(mode.clone()));
    }
    let picks = ui::can_pick(ctx);
    if picks && launch.issue == Some(ItemChoice::Pick) {
        return Ok(ModeChoice::Named(modes::ModeName::PLAN));
    }
    let labelled = match found {
        Some(repo) => item_mode(ctx, launch, repo, (settings, modes)).await?,
        None => None,
    };
    Ok(labelled.unwrap_or_else(|| match picks {
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
    choice.chosen(ctx, modes, fit(launch)?)
}

/// The spaces a mode picked for `launch` must start: of the kind its
/// arguments other than the mode fit, and in a repository where they
/// [require](requires_repository) one.
fn fit(launch: &Launch) -> Result<ModeFit> {
    Ok(ModeFit {
        kind: launches::fitting_kind(launch),
        repository: requires_repository(launch)?.then_some(true),
    })
}

/// Whether the arguments of `launch` call for a repository whatever its
/// mode: they name one, an existing workspace, an issue, which only a
/// space in a repository starts on, or what only a workspace takes.
pub(super) fn requires_repository(launch: &Launch) -> Result<bool> {
    Ok(launch.in_workspace.is_some()
        || launch.issue.is_some()
        || launches::fitting_kind(launch).is_some()
        || launches::names_repository(launch)?
        || launches::named_issue(launch)?.is_some())
}

/// The space `mode` of `launch` starts among the `modes` the start may
/// take, which says what a terminal picks for it; none for a setup's,
/// which its setup settles.
pub(super) fn configured(
    launch: &Launch,
    mode: &modes::ModeName,
    modes: &ModeShapes,
) -> Result<Option<modes::ModeShape>> {
    match launch.setup {
        Some(_) => Ok(None),
        None => modes::get(modes, mode).copied().map(Some),
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

/// The mode the labels of the issue `launch` names in `repo` call for, by
/// [`launches::labelled_mode`] among the [`labelled`] modes of `settings`;
/// a pick on a terminal where they call for several, and refused off one;
/// none without such an issue or a mode with a label it carries.
async fn item_mode(
    ctx: &Context,
    launch: &Launch,
    repo: &Repository,
    (settings, modes): (&Effective, &ModeShapes),
) -> Result<Option<ModeChoice>> {
    let Some(input) = launches::named_issue(launch)? else {
        return Ok(None);
    };
    let labelled = labelled(launch, settings, modes)?;
    if labelled.is_empty() {
        return Ok(None);
    }
    let context = issues::read_context(ctx, repo).await?;
    let issue = issues::load(ctx, context, input.clone()).await?;
    match launches::labelled_mode(labelled, &issue.labels) {
        Ok(mode) => Ok(mode.map(ModeChoice::Named)),
        Err(_) if ui::can_pick(ctx) => Ok(Some(ModeChoice::Pick)),
        Err(several) => anyhow::bail!("issue {input}: {several}"),
    }
}

/// Each mode of `settings` with a label, by name with its label, among the
/// `modes` the start may take that start the spaces the arguments of
/// `launch` other than the mode [`fit`].
fn labelled<'a>(
    launch: &Launch,
    settings: &'a Effective,
    modes: &ModeShapes,
) -> Result<Vec<(&'a str, &'a str)>> {
    let fit = fit(launch)?;
    let fits = |name: &str| modes.get(name).is_some_and(|shape| fit.admits(shape));
    Ok((settings.modes.iter())
        .filter(|(name, _)| fits(name))
        .filter_map(|(name, mode)| Some((name.as_str(), mode.label.as_deref()?)))
        .collect())
}

#[cfg(test)]
mod tests;
