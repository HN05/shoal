//! `shoal sleep`.
use anyhow::Result;

use crate::{
    cli::{
        args::coordination::SleepTimerArgs,
        context::Context,
        ui::{self, Acting, Fallback},
    },
    client::sleep,
    state::Sleep,
    time::UnixTime,
};
use shoal_protocol::{
    sleep::{Slept, WakeTimer},
    space_names::SpaceSelector,
};

pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    sleep: Option<Sleep>,
    timer: Option<WakeTimer>,
) -> Result<i32> {
    let sleeping = sleeping();
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory(&sleeping)).await?;
    let slept = sleep::set(space, sleep, timer).send(&ctx.paths).await?;
    ctx.show(&slept, |slept| outln!("{}", summary(slept)))?;
    Ok(0)
}

pub(super) fn timer(args: &SleepTimerArgs, message: &[String], now: UnixTime) -> Option<WakeTimer> {
    let at = match (args.after, args.until) {
        (Some(after), _) => now + after.duration(),
        (None, Some(until)) => until.next_local(now),
        (None, None) => return None,
    };
    let message = Some(message.join(" ")).filter(|message| !message.is_empty());
    Some(WakeTimer { at, message })
}

fn summary(slept: &Slept) -> String {
    let space = &slept.space;
    let sleep = match (slept.sleep, slept.idle_wake_minutes) {
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
    };
    match slept.wake_at {
        Some(at) => format!("{sleep} Its timer wakes it at {}.", at.local_minutes()),
        None => sleep,
    }
}

/// How it sleeps decides nothing of which spaces it reaches.
pub(super) fn sleeping() -> Acting {
    Acting::request(|space| sleep::set(space, None, None))
}

#[cfg(test)]
mod tests;
