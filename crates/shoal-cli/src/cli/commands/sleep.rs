//! `shoal sleep`: put a space's agent to sleep from the end of its turn,
//! until its next turn starts or after every turn until `--undo`, which
//! wakes it.
use anyhow::Result;

use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    client::{
        sleep::{self, Slept},
        space_names::SpaceSelector,
    },
    state::Sleep,
};

/// Let the agent of the space `space` names, else the current one, sleep
/// as `sleep` says, or with none end its sleep and wake it.
pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    sleep: Option<Sleep>,
) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let slept = sleep::set(&ctx.paths, space, sleep).await?;
    ctx.show(&slept, |slept| outln!("{}", summary(slept)))?;
    Ok(0)
}

/// How long the agent sleeps after `slept`, or how Shoal wakes it.
fn summary(slept: &Slept) -> String {
    let space = &slept.space;
    match (slept.sleep, slept.idle_wake_minutes) {
        (Some(Sleep::Turn), _) => {
            format!(
                "{space}: the agent sleeps from the end of its turn until its next turn starts."
            )
        }
        (Some(Sleep::Permanent), _) => {
            format!("{space}: the agent sleeps after every turn until shoal sleep --undo.")
        }
        (None, Some(minutes)) => {
            format!("{space}: awake; Shoal wakes the agent after {minutes} idle minutes.")
        }
        (None, None) => format!("{space}: awake."),
    }
}
