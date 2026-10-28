//! Herdr's pane query and close protocol; caller supplies the recorded server.
use super::records::Place;
use crate::subprocess::Run;
use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use std::time::Duration;
use tokio::process::Command;

pub(super) fn current_pane() -> Result<Option<String>> {
    if std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return Ok(None);
    }
    Ok(Some(
        std::env::var("HERDR_PANE_ID").context("Herdr did not name its pane")?,
    ))
}

pub(super) async fn verified(mut place: Place) -> Result<Place> {
    place.terminal = terminal(&place)
        .await?
        .context("Herdr pane disappeared before ownership was recorded")?;
    Ok(place)
}

fn command(place: &Place, operation: &str) -> Command {
    let mut command = Command::new("herdr");
    command
        .env("HERDR_SOCKET_PATH", &place.socket)
        .args(["pane", operation, &place.pane]);
    command
}

pub(super) async fn close_pane(place: &Place) -> Result<()> {
    let result = Run::new(command(place, "close"))
        .timeout(Duration::from_secs(30))
        .checked()
        .await;
    if result.is_err() && terminal(place).await?.is_none() {
        return Ok(());
    }
    result.with_context(|| format!("close Herdr pane {}", place.pane))?;
    Ok(())
}

pub(super) async fn terminal(place: &Place) -> Result<Option<String>> {
    let output = Run::new(command(place, "get"))
        .timeout(Duration::from_secs(30))
        .capture()
        .await?;
    if output.status.success() {
        let response: PaneResponse =
            serde_json::from_slice(&output.stdout).context("invalid Herdr pane response")?;
        ensure!(
            response.result.pane.pane_id == place.pane
                && !response.result.pane.terminal_id.is_empty(),
            "Herdr did not verify the pane's terminal identity"
        );
        return Ok(Some(response.result.pane.terminal_id));
    }
    if missing(&output.stdout) || missing(&output.stderr) {
        return Ok(None);
    }
    bail!(
        "query Herdr pane {}: {} {}",
        place.pane,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn missing(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Failure>(bytes)
        .is_ok_and(|response| response.error.code == "pane_not_found")
}

#[derive(Deserialize)]
struct PaneResponse {
    result: PaneResult,
}
#[derive(Deserialize)]
struct PaneResult {
    pane: Pane,
}
#[derive(Deserialize)]
struct Pane {
    pane_id: String,
    terminal_id: String,
}
#[derive(Deserialize)]
struct Failure {
    error: FailureCode,
}
#[derive(Deserialize)]
struct FailureCode {
    code: String,
}

#[cfg(test)]
mod tests;
