//! The spaces each command last acted on, so `-` can name the previous one,
//! read from and kept in [`shoal_workers::history`].
use shoal_ids::space_ids::SpaceId;

use anyhow::{Context as _, Result, ensure};
use shoal_workers::history::{self, History, keeps_history};

use crate::{
    cli::context::Context,
    model::{Space, Spaces},
};
use shoal_protocol::space_names::SpaceSelector;

/// Whether `selector` is `-`, which names the previous space.
pub fn is_previous(selector: &SpaceSelector) -> bool {
    *selector == SpaceSelector::Previous
}

/// The space the command last acted on other than the current workspace
/// and the space this process runs in.
pub async fn previous(ctx: &Context) -> Result<SpaceId> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    previous_among(ctx, &spaces, &[])
}

/// [`previous`] among `spaces` and `elsewhere`, other joined machines'
/// spaces, for a command that returns only to some.
pub fn previous_among(ctx: &Context, spaces: &Spaces, elsewhere: &[SpaceId]) -> Result<SpaceId> {
    let command = command(ctx)?;
    let current = history::current(spaces)?;
    let listed: Vec<&SpaceId> = (spaces.iter().map(|space| &space.id))
        .chain(elsewhere)
        .collect();
    let current: Vec<&SpaceId> = current.iter().collect();
    History::load(&ctx.paths)
        .previous(command, &listed, &current)
        .with_context(|| format!("no previous space for `shoal {command}`"))
}

/// The history the command keeps, outside workspace processes, which keep
/// none.
fn command(ctx: &Context) -> Result<&str> {
    let command = ctx
        .recent
        .as_deref()
        .context("this command does not accept - for a previous space")?;
    ensure!(
        keeps_history(&ctx.paths),
        "workspace processes have no previous space"
    );
    Ok(command)
}

/// Remember that the command acted on the space `selector` names.
pub async fn record(ctx: &Context, selector: &SpaceSelector) {
    record_where(ctx, selector, |_| true).await;
}

/// [`record`] for a command that acts only on the spaces `accepts` takes,
/// so one it refuses stays out of its history.
pub async fn record_where(
    ctx: &Context,
    selector: &SpaceSelector,
    accepts: impl Fn(&Space) -> bool,
) {
    if let Some(command) = ctx.recent.as_deref() {
        history::remember(&ctx.paths, command, selector, false, accepts).await;
    }
}

/// Remember that the command moved from the current space to the one
/// `selector` names, so `-` returns to the one it left.
pub async fn record_move(ctx: &Context, selector: &SpaceSelector) {
    if let Some(command) = ctx.recent.as_deref() {
        history::remember(&ctx.paths, command, selector, true, |_| true).await;
    }
}
