//! `shoal demo screen` and `shoal demo send`: reading a demo's screen and
//! typing into it.
use anyhow::Result;

use super::lookup::named;
use crate::cli::{context::Context, terminal};
use crate::client::output::demo::{Screen, Sent};
use crate::client::space_names::SpaceSelector;

pub(super) async fn screen(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: &str,
    lines: usize,
) -> Result<i32> {
    let demo = named(ctx, workspace, name).await?;
    let screen = terminal::demo_screen(&demo.launch).await?;
    let text = last_lines(&screen, lines).unwrap_or_default();
    let shown = Screen {
        name: name.to_owned(),
        screen: text.clone(),
    };
    ctx.show(&shown, |_| outln!("{text}"))?;
    Ok(0)
}

pub(super) async fn send(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: &str,
    keys: &str,
) -> Result<i32> {
    let demo = named(ctx, workspace, name).await?;
    terminal::send_demo_keys(&demo.launch, keys).await?;
    ctx.emit(&format!("Sent to demo {name}"), Sent { sent: true })?;
    Ok(0)
}

/// The last `count` lines of `screen` with trailing blank lines dropped.
pub(super) fn last_lines(screen: &str, count: usize) -> Option<String> {
    let lines: Vec<&str> = screen.trim_end().lines().collect();
    let start = lines.len().saturating_sub(count);
    let text = lines[start..].join("\n");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests;
