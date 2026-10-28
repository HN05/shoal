//! Demos in Herdr tabs: each runs in a background tab whose pane is the
//! demo's place, and the tab closes once the demo stops.
use anyhow::{Context as _, Result};
use serde::Deserialize;
use tokio::process::Command;

use super::{created, herdr, shell_command, tab_command};
use crate::{cli::terminal::Demo, subprocess::Run};

/// Lines of recent output a screen read returns.
const SCREEN_LINES: &str = "500";

/// Open a background tab for the demo and run its worker in the tab's pane,
/// returning the pane.
pub(super) async fn start(demo: &Demo<'_>) -> Result<String> {
    let origin = demo.origin.context("Shoal does not run in a Herdr pane")?;
    let pane = created(tab_command(origin, demo.cwd, demo.label, false, demo.env)?)
        .await?
        .root_pane
        .pane_id;
    let mut run = herdr(origin);
    run.args(["pane", "run", &pane]).arg(format!(
        "{} && exit",
        shell_command(&(demo.worker)(&pane)?)?
    ));
    Run::new(run)
        .checked()
        .await
        .context("run the demo in its Herdr tab (tab retained)")?;
    Ok(pane)
}

/// Focus the tab holding the demo's pane.
pub(super) async fn open(pane: &str) -> Result<()> {
    let mut get = Command::new("herdr");
    get.args(["pane", "get", pane]);
    let output = Run::new(get)
        .checked()
        .await
        .context("find the demo's Herdr tab")?;
    let info: Response<PaneResult> =
        serde_json::from_slice(&output.stdout).context("invalid Herdr pane response")?;
    let mut focus = Command::new("herdr");
    focus.args(["tab", "focus", &info.result.pane.tab_id]);
    Run::new(focus)
        .checked()
        .await
        .context("focus the demo's Herdr tab")?;
    Ok(())
}

pub(super) async fn screen(pane: &str) -> Result<String> {
    let mut read = Command::new("herdr");
    read.args([
        "pane",
        "read",
        pane,
        "--source",
        "recent",
        "--lines",
        SCREEN_LINES,
        "--format",
        "text",
    ]);
    let output = Run::new(read)
        .checked()
        .await
        .context("read the demo's Herdr pane")?;
    Ok(read_text(&output.stdout))
}

pub(super) async fn send(pane: &str, keys: &str) -> Result<()> {
    let mut send = Command::new("herdr");
    send.args(["pane", "send-text", pane, keys]);
    Run::new(send)
        .checked()
        .await
        .context("type into the demo's Herdr pane")?;
    Ok(())
}

/// The text of a pane read: Herdr's JSON response, or its plain output.
fn read_text(stdout: &[u8]) -> String {
    serde_json::from_slice::<Response<ReadResult>>(stdout).map_or_else(
        |_| String::from_utf8_lossy(stdout).into_owned(),
        |response| response.result.read.text,
    )
}

#[derive(Deserialize)]
struct Response<T> {
    result: T,
}
#[derive(Deserialize)]
struct PaneResult {
    pane: Pane,
}
#[derive(Deserialize)]
struct Pane {
    tab_id: String,
}
#[derive(Deserialize)]
struct ReadResult {
    read: Read,
}
#[derive(Deserialize)]
struct Read {
    text: String,
}

#[cfg(test)]
mod tests;
