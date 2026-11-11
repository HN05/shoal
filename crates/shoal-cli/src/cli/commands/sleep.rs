//! `shoal sleep`: keep Shoal from waking a space's idle agent, until its
//! next turn starts or until `--undo`.
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
/// as `sleep` says, or end its sleep with none.
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

/// How long Shoal leaves the agent idle after `slept`.
fn summary(slept: &Slept) -> String {
    let space = &slept.space;
    match (slept.sleep, slept.idle_wake_minutes) {
        (Some(Sleep::Turn), _) => {
            format!("{space}: no idle wake until the agent's next turn starts.")
        }
        (Some(Sleep::Permanent), _) => {
            format!("{space}: no idle wakes until shoal sleep --undo.")
        }
        (None, Some(minutes)) => {
            format!("{space}: Shoal wakes the agent after {minutes} idle minutes.")
        }
        (None, None) => format!("{space}: the space's mode never wakes an idle agent."),
    }
}
