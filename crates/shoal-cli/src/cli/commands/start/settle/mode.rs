//! The mode a start's agent starts in.
use anyhow::Result;
use shoal_protocol::launches::{LabelledMode, Launch};

use crate::{
    cli::{
        args::create::StartArgs,
        commands::start::{launch_settings, start_modes},
        context::Context,
        ui::{self, ModeChoice},
    },
    config::modes::{self, ModeShapes},
    launches::{self, fit, labels},
    model::SpaceKind,
};

/// Otherwise a terminal picks one once the daemon has checked the rest, and off
/// a terminal the [`launches::default_mode`] applies, or labels that call for
/// several refuse the start.
pub(super) async fn decided(
    ctx: &Context,
    launch: &Launch,
    modes: &ModeShapes,
) -> Result<ModeChoice> {
    if let Some(mode) = &launch.mode {
        return Ok(ModeChoice::Named(mode.clone()));
    }
    let labelled = labelled(ctx, launch, modes).await?;
    Ok(match (labelled, ui::can_pick(ctx)) {
        (LabelledMode::Called(mode), _) => ModeChoice::Named(mode),
        (_, true) => ModeChoice::Pick,
        (LabelledMode::Several(several), false) => return Err(several.into()),
        (LabelledMode::Unlabelled, false) => ModeChoice::Named(launches::default_mode(launch)),
    })
}

/// Unlabelled without asking where the launch names no item or none of the
/// `modes` has a label.
async fn labelled(ctx: &Context, launch: &Launch, modes: &ModeShapes) -> Result<LabelledMode> {
    if !labels::names_item(launch)? || labels::labelled(launch, modes)?.is_empty() {
        return Ok(LabelledMode::Unlabelled);
    }
    crate::client::launches::labelled_mode(&ctx.paths, launch.clone()).await
}

/// Refused before either picker asks anything.
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

/// A setup's mode is built in, which no config defines.
pub(super) fn chosen(
    ctx: &Context,
    choice: ModeChoice,
    launch: &Launch,
    modes: &ModeShapes,
) -> Result<modes::ModeName> {
    if let (ModeChoice::Named(mode), Some(_)) = (&choice, launch.setup) {
        return Ok(mode.clone());
    }
    choice.chosen(ctx, modes, fit::fit(launch)?)
}

/// None for a setup's mode, which its setup settles.
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

/// A setup's mode starts where its setup says.
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

pub(super) fn starts_workspace(
    launch: &Launch,
    mode: &modes::ModeName,
    modes: &ModeShapes,
) -> Result<bool> {
    let configured = configured(launch, mode, modes)?;
    Ok(configured.is_some_and(|mode| mode.kind == SpaceKind::Workspace))
}

#[cfg(test)]
mod tests;
