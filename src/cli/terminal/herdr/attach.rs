//! Reaching the tab of a recorded pane: `shoal attach` finds it while the
//! pane is open, and showing it focuses the tab and, from a terminal outside
//! Herdr, attaches a Herdr client to the tab's session.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

use super::{TabInfo, failed_with, herdr, in_herdr};
use crate::{
    cli::terminal::Session,
    sessions::{HostName, Origin},
    subprocess::Run,
};

/// The variables naming the Herdr server a pane runs in, most specific first.
const SERVER_VARS: &[&str] = &["HERDR_SOCKET_PATH", "HERDR_SESSION"];

/// The session Herdr attaches when a pane recorded none.
const DEFAULT_SESSION: &str = "default";

/// The session at `place`, named after its tab, while its pane is open.
pub(super) async fn session_at(place: &Origin) -> Result<Option<Session>> {
    let Some(pane) = pane(place).await? else {
        return Ok(None);
    };
    let label = tab_label(place, &pane.tab_id).await?;
    Ok(Some(Session {
        name: label.unwrap_or(pane.tab_id),
        agent: None,
        clients: 0,
        host: HostName::Herdr.as_str(),
        place: Some(place.clone()),
    }))
}

/// Focus the tab of the pane at `place`, attaching this terminal to the
/// tab's Herdr session when it runs outside Herdr.
pub(super) async fn reveal(place: &Origin) -> Result<()> {
    let client = client_session(place)?;
    let pane = pane(place).await?.context("the Herdr pane is gone")?;
    focus_tab(place, &pane.tab_id).await?;
    match client {
        Some(session) => attach_client(session).await,
        None => Ok(()),
    }
}

/// The Herdr session this terminal attaches to show `place`, or `None`
/// when this terminal already shows it, as a pane of the same server does.
fn client_session(place: &Origin) -> Result<Option<&str>> {
    let session = place
        .data
        .get("HERDR_SESSION")
        .map_or(DEFAULT_SESSION, String::as_str);
    if !in_herdr() {
        return Ok(Some(session));
    }
    if same_server(&place.data, |name| std::env::var(name).ok()) {
        return Ok(None);
    }
    bail!(
        "the Herdr tab runs in Herdr session {session}; open it there or attach from outside Herdr"
    )
}

/// Whether `current` names the server `recorded` does, by the first
/// variable both have. A pane that recorded none shares the current server;
/// one that recorded a variable the current process lacks does not.
fn same_server(
    recorded: &BTreeMap<String, String>,
    current: impl Fn(&str) -> Option<String>,
) -> bool {
    match SERVER_VARS
        .iter()
        .find_map(|name| Some((recorded.get(*name)?, current(name)?)))
    {
        Some((recorded, current)) => *recorded == current,
        None => !SERVER_VARS.iter().any(|name| recorded.contains_key(*name)),
    }
}

/// The pane at `place`, or `None` when Herdr reports it gone.
async fn pane(place: &Origin) -> Result<Option<Pane>> {
    let mut get = herdr(place);
    get.args(["pane", "get", place.id()]);
    let output = Run::new(get).capture().await.context("query Herdr pane")?;
    if output.status.success() {
        let info: PaneInfo =
            serde_json::from_slice(&output.stdout).context("invalid Herdr pane response")?;
        return Ok(Some(info.result.pane));
    }
    if failed_with(&output.stdout, "pane_not_found") {
        return Ok(None);
    }
    bail!(
        "herdr pane get {} failed: {}",
        place.id(),
        String::from_utf8_lossy(&output.stdout).trim()
    )
}

/// The label of `tab` on the server of `place`.
async fn tab_label(place: &Origin, tab: &str) -> Result<Option<String>> {
    let mut get = herdr(place);
    get.args(["tab", "get", tab]);
    let output = Run::new(get).checked().await.context("query Herdr tab")?;
    let info: TabInfo =
        serde_json::from_slice(&output.stdout).context("invalid Herdr tab response")?;
    Ok(info.result.tab.label.filter(|label| !label.is_empty()))
}

async fn focus_tab(place: &Origin, tab: &str) -> Result<()> {
    let mut focus = herdr(place);
    focus.args(["tab", "focus", tab]);
    Run::new(focus)
        .checked()
        .await
        .context("focus the Herdr tab")?;
    Ok(())
}

/// Attach this terminal to Herdr `session` until the user detaches.
async fn attach_client(session: &str) -> Result<()> {
    let status = Command::new("herdr")
        .args(["session", "attach", session])
        .status()
        .await
        .context("run herdr")?;
    crate::execution::reset_input_modes();
    if !status.success() {
        bail!("herdr exited with {status}");
    }
    Ok(())
}

#[derive(Deserialize)]
struct PaneInfo {
    result: PaneResult,
}
#[derive(Deserialize)]
struct PaneResult {
    pane: Pane,
}
#[derive(Deserialize)]
struct Pane {
    tab_id: String,
}

#[cfg(test)]
mod tests;
