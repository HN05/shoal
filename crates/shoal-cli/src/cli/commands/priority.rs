//! `shoal priority`: set how much a space's work matters, which ranks its
//! queued work ahead of everything else.
use anyhow::Result;

use crate::{
    cli::{
        context::Context,
        ui::{self, Acting, Fallback},
    },
    client::priority,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_schedule::priority::Priority;

/// Set `value` on the space `space` names, else the current one.
pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    value: Priority,
) -> Result<i32> {
    let setting = Acting::request(move |space| priority::set(space, value));
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory(&setting)).await?;
    let set = priority::set(space, value).send(&ctx.paths).await?;
    ctx.show(&set, |set| {
        outln!("{}: priority {}.", set.space, set.priority)
    })?;
    Ok(0)
}
