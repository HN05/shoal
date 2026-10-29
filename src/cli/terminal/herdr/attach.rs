//! Reaching the tab of a recorded pane: `shoal attach` finds it while the
//! pane is open, and showing it focuses the tab and, from a terminal outside
//! Herdr, attaches a Herdr client to the tab's session.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

use super::{Pane, TabInfo, TabState, herdr};
use crate::{
    cli::terminal::Session,
    sessions::{Origin, herdr::SERVER_VARS},
    subprocess::Run,
};

/// The session at `place`, named after its tab, while its pane is open.
/// Herdr does not count its clients, so the session has one while its tab
/// is the focused one, which a connected client shows.
pub(super) async fn session_at(place: &Origin) -> Result<Option<Session>> {
    let Some(pane) = pane(place).await? else {
        return Ok(None);
    };
    let tab = tab(place, &pane.tab_id).await?;
    let label = tab.label.filter(|label| !label.is_empty());
    let name = label.unwrap_or(pane.tab_id);
    Ok(Some(Session::open(name, u32::from(tab.focused), place)))
}

/// Focus the tab of the pane at `place`, attaching this terminal to the
/// tab's Herdr session when it runs outside Herdr.
pub(super) async fn reveal(place: &Origin) -> Result<()> {
    let client = client_session(place, |name| std::env::var(name).ok())?;
    let pane = pane(place).await?.context("the Herdr pane is gone")?;
    focus_tab(place, &pane.tab_id).await?;
    match client {
        Some(session) => attach_client(&session.name().await?).await,
        None => Ok(()),
    }
}

/// The Herdr session holding a server, as a record of the server's
/// variables names it.
#[derive(Debug, PartialEq, Eq)]
enum HerdrSession<'a> {
    Named(&'a str),
    /// The session whose server listens on this socket.
    AtSocket(&'a str),
    /// Herdr's default session, which a record of no variables reaches.
    Default,
}

impl HerdrSession<'_> {
    /// The session's name, from Herdr's list of sessions unless the record
    /// names it.
    async fn name(&self) -> Result<String> {
        if let Self::Named(name) = self {
            return Ok((*name).to_owned());
        }
        let mut list = Command::new("herdr");
        list.args(["session", "list", "--json"]);
        let output = Run::new(list)
            .checked()
            .await
            .context("list Herdr sessions")?;
        let listed: Sessions =
            serde_json::from_slice(&output.stdout).context("invalid Herdr session list")?;
        self.find(&listed.sessions)
            .map(|session| session.name.clone())
            .context("Herdr lists no session holding the tab; open it in Herdr")
    }

    /// The one of `sessions` this is.
    fn find<'a>(&self, sessions: &'a [ListedSession]) -> Option<&'a ListedSession> {
        sessions.iter().find(|session| match self {
            Self::Named(name) => session.name == *name,
            Self::AtSocket(socket) => session.socket_path.as_deref() == Some(*socket),
            Self::Default => session.default,
        })
    }
}

#[derive(Deserialize)]
struct Sessions {
    sessions: Vec<ListedSession>,
}
#[derive(Deserialize)]
struct ListedSession {
    name: String,
    #[serde(default)]
    default: bool,
    socket_path: Option<String>,
}

/// The Herdr session a terminal with the variables `current` gives attaches
/// to show `place`, or `None` when it already shows it, as a pane of the
/// same server does.
fn client_session(
    place: &Origin,
    current: impl Fn(&str) -> Option<String>,
) -> Result<Option<HerdrSession<'_>>> {
    let session = place.data.get("HERDR_SESSION").map(String::as_str);
    if current("HERDR_ENV").as_deref() != Some("1") {
        let socket = place.data.get("HERDR_SOCKET_PATH").map(String::as_str);
        return Ok(Some(match (session, socket) {
            (Some(session), _) => HerdrSession::Named(session),
            (None, Some(socket)) => HerdrSession::AtSocket(socket),
            (None, None) => HerdrSession::Default,
        }));
    }
    if same_server(&place.data, current) {
        return Ok(None);
    }
    match session {
        Some(session) => bail!(
            "the Herdr tab runs in Herdr session {session}; open it there or attach from outside Herdr"
        ),
        None => bail!("the Herdr tab runs in another Herdr session; open it there"),
    }
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
    super::pane(herdr(place), place.id()).await
}

/// `tab` on the server of `place`.
async fn tab(place: &Origin, tab: &str) -> Result<TabState> {
    let mut get = herdr(place);
    get.args(["tab", "get", tab]);
    let output = Run::new(get).checked().await.context("query Herdr tab")?;
    let info: TabInfo =
        serde_json::from_slice(&output.stdout).context("invalid Herdr tab response")?;
    Ok(info.result.tab)
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

#[cfg(test)]
mod tests;
