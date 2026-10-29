//! Demos in Herdr tabs: each runs in a background tab whose pane is the
//! demo's place, and the tab closes once the demo stops.
use anyhow::{Context as _, Result};
use serde::Deserialize;
use tokio::process::Command;

use super::{created, herdr, shell_command, tab_command};
use crate::{cli::terminal::Demo, sessions::Origin, subprocess::Run};

/// Lines of recent output a screen read returns.
const SCREEN_LINES: &str = "500";

/// Open a background tab for the demo and run its worker in the tab's pane,
/// returning the pane.
pub(super) async fn start(demo: &Demo<'_>) -> Result<Origin> {
    let origin = demo.origin.context("Shoal does not run in a Herdr pane")?;
    let pane = created(tab_command(origin, demo.cwd, demo.label, false, demo.env)?)
        .await?
        .root_pane
        .pane_id;
    let place = Origin {
        place: Some(pane),
        owned: true,
        ..origin.clone()
    };
    let mut run = herdr(origin);
    run.args(["pane", "run", place.id()]).arg(format!(
        "{} && exit",
        shell_command(&(demo.worker)(&place)?)?
    ));
    Run::new(run)
        .checked()
        .await
        .context("run the demo in its Herdr tab (tab retained)")?;
    Ok(place)
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
struct ReadResult {
    read: Read,
}
#[derive(Deserialize)]
struct Read {
    text: String,
}

#[cfg(test)]
mod tests;
