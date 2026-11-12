//! `shoal priority`: set how much a space's work matters, which ranks its
//! queued work ahead of everything else.
use anyhow::Result;

use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    client::{
        priority::{self, Priority},
        space_names::SpaceSelector,
    },
};

/// Set `value` on the space `space` names, else the current one.
pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    value: Priority,
) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let set = priority::set(&ctx.paths, space, value).await?;
    ctx.show(&set, |set| {
        outln!("{}: priority {}.", set.space, set.priority)
    })?;
    Ok(0)
}
