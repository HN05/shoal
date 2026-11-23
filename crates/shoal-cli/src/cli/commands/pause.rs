//! `shoal pause` and `shoal unpause`: stop all work on a machine, sending
//! what it can to other machines of the hub, and admit work there again.
use anyhow::Result;
use shoal_protocol::pause::{Paused, Unpaused};

use crate::cli::{context::Context, output::Style};

/// Pause the joined machine `machine` names, or this one.
pub(super) async fn pause(ctx: &Context, machine: Option<String>) -> Result<i32> {
    let paused = crate::client::pause::pause(&ctx.paths, machine.clone()).await?;
    let message = paused_message(&paused, machine.as_deref());
    ctx.emit_styled(Style::Success, &message, &paused)?;
    Ok(0)
}

/// Unpause the joined machine `machine` names, or this one.
pub(super) async fn unpause(ctx: &Context, machine: Option<String>) -> Result<i32> {
    let unpaused = crate::client::pause::unpause(&ctx.paths, machine.clone()).await?;
    let message = unpaused_message(&unpaused, machine.as_deref());
    ctx.emit_styled(Style::Success, &message, &unpaused)?;
    Ok(0)
}

/// The machine a command acts on: the one `machine` names, or this one.
fn which(machine: Option<&str>) -> String {
    match machine {
        Some(machine) => format!("machine {machine}"),
        None => "this machine".into(),
    }
}

/// What `shoal pause` says of `paused` on the machine `machine` names.
fn paused_message(paused: &Paused, machine: Option<&str>) -> String {
    let which = which(machine);
    let since = paused.since.local_minutes();
    let stopped = match paused.stopped.len() {
        0 => String::new(),
        1 => "; stopped work in 1 space, which an alert names with where it went".into(),
        count => {
            format!("; stopped work in {count} spaces, which an alert names with where each went")
        }
    };
    format!("Paused {which} since {since}{stopped}; it admits no new work until shoal unpause")
}

/// What `shoal unpause` says of `unpaused` on the machine `machine` names.
fn unpaused_message(unpaused: &Unpaused, machine: Option<&str>) -> String {
    let which = which(machine);
    match unpaused.paused_since {
        Some(since) => format!(
            "Unpaused {which}, paused since {}; it admits work again and restores the agents the pause stopped there",
            since.local_minutes()
        ),
        None => format!("{} was not paused", capitalized(&which)),
    }
}

/// `text` with its first letter capitalized.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
