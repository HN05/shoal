//! The mode an addition starts its agent in: named or picked.
use anyhow::Result;

use super::agent::AddSettings;
use crate::{
    cli::{context::Context, ui::ModeChoice},
    model::SpaceKind,
};

/// The mode `choice` names or picks among those that start in a
/// workspace; `None`, the default mode, without one. A named mode is
/// checked with the plan, so only a pick reads the settings here.
pub(super) async fn choose_add_mode(
    ctx: &Context,
    settings: &AddSettings<'_>,
    choice: Option<ModeChoice>,
) -> Result<Option<String>> {
    match choice {
        None => Ok(None),
        Some(ModeChoice::Named(mode)) => Ok(Some(mode)),
        Some(pick) => {
            let modes = &settings.get().await?.modes;
            pick.chosen(ctx, modes, Some(SpaceKind::Workspace))
                .map(Some)
        }
    }
}
