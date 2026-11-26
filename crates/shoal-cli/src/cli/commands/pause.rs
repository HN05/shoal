//! `shoal pause` and `shoal unpause`.
use anyhow::Result;
use shoal_protocol::pause::{Paused, Unpaused};

use crate::cli::{context::Context, output::Style};

pub(super) async fn pause(ctx: &Context, machine: Option<String>) -> Result<i32> {
    let paused = crate::client::pause::pause(&ctx.paths, machine.clone()).await?;
    let message = paused_message(&paused, machine.as_deref());
    ctx.emit_styled(Style::Success, &message, &paused)?;
    Ok(0)
}

pub(super) async fn unpause(ctx: &Context, machine: Option<String>) -> Result<i32> {
    let unpaused = crate::client::pause::unpause(&ctx.paths, machine.clone()).await?;
    let message = unpaused_message(&unpaused, machine.as_deref());
    ctx.emit_styled(Style::Success, &message, &unpaused)?;
    Ok(0)
}

fn which(machine: Option<&str>) -> String {
    match machine {
        Some(machine) => format!("machine {machine}"),
        None => "this machine".into(),
    }
}

fn paused_message(paused: &Paused, machine: Option<&str>) -> String {
    let which = which(machine);
    let since = paused.since.local_minutes();
    format!(
        "Paused {which} since {since}; its work stops, an alert naming where each space went, and it admits no new work until shoal unpause"
    )
}

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

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
