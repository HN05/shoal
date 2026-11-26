//! `shoal priority`.
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

pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    value: Priority,
) -> Result<i32> {
    let setting = setting(value);
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory(&setting)).await?;
    let set = priority::set(space, value).send(&ctx.paths).await?;
    ctx.show(&set, |set| {
        outln!("{}: priority {}.", set.space, set.priority)
    })?;
    Ok(0)
}

pub(super) fn setting(value: Priority) -> Acting {
    Acting::request(move |space| priority::set(space, value))
}
