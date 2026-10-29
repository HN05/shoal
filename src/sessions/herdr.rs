//! The Herdr CLI against one Herdr server, for the terminal host and the
//! daemon's cleanup alike. Herdr never reuses a session's tab or pane IDs
//! and keeps them when its server restarts, so a pane ID identifies a place.
use std::{collections::BTreeMap, path::Path, process::Output};

use anyhow::{Context as _, Result, bail};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use tokio::process::Command;

use crate::subprocess::Run;

/// The variable naming the `herdr` that reaches the server of a pane.
const BIN_PATH: &str = "HERDR_BIN_PATH";

/// The variables naming the Herdr server a pane runs in, most specific first.
pub(crate) const SERVER_VARS: &[&str] = &["HERDR_SOCKET_PATH", "HERDR_SESSION"];

/// The variables that choose the server a process reaches and its `herdr`.
pub(crate) const SERVER_SELECTION: &[&str] = &["HERDR_SOCKET_PATH", "HERDR_SESSION", BIN_PATH];

/// The server whose variables `data` records, by the variable that routes
/// to it: the most specific one present, as `herdr` reads them.
pub(crate) fn server(data: &BTreeMap<String, String>) -> String {
    SERVER_VARS
        .iter()
        .find_map(|name| Some(format!("{name}={}", data.get(*name)?)))
        .unwrap_or_default()
}

/// A `herdr` command reaching the server whose variables `data` records:
/// the `herdr` Herdr named there, otherwise the one on PATH. Variables this
/// process inherited that name another server give way, so a record without
/// them reaches Herdr's default server rather than this process's.
pub(crate) fn command(data: &BTreeMap<String, String>) -> Command {
    let program = data
        .get(BIN_PATH)
        .filter(|path| Path::new(path).is_file())
        .map_or("herdr", String::as_str);
    let mut command = Command::new(program);
    for name in SERVER_VARS {
        command.env_remove(name);
    }
    command.envs(data);
    command
}

/// Close `pane` of the server `data` records, and nothing else: Herdr
/// closes a tab with its last pane, so the user's other panes in it stay.
/// A pane already gone is closed.
async fn close_pane(data: &BTreeMap<String, String>, pane: &str) -> Result<()> {
    let mut close = command(data);
    close.args(["pane", "close", pane]);
    let output = Run::new(close)
        .capture()
        .await
        .with_context(|| format!("close Herdr pane {pane}"))?;
    if output.status.success() || not_found(&output, "pane_not_found") {
        return Ok(());
    }
    bail!(
        "cannot close Herdr pane {pane}: {}",
        crate::subprocess::diagnostic(&output.stderr).trim()
    )
}

/// Whether a failed command's `output` has Herdr report what it named
/// missing, with the error `code` it gives for that, such as
/// `tab_not_found`. Herdr prints the error as JSON on stderr. A missing
/// server is not a missing tab, since a replacement server keeps its tabs.
pub(crate) fn not_found(output: &Output, code: &str) -> bool {
    serde_json::from_slice::<Failure>(&output.stderr)
        .is_ok_and(|failure| failure.error.code == code)
}

#[derive(Deserialize)]
struct Failure {
    error: FailureError,
}
#[derive(Deserialize)]
struct FailureError {
    code: String,
}

/// Herdr panes carry no Shoal labels, so the daemon's records of the panes
/// Shoal created are the only proof of ownership: discovery finds none, and
/// a close needs a recorded pane, which is Shoal's while it exists.
pub(super) struct Cleanup;

impl super::Host for Cleanup {
    fn name(&self) -> super::HostName {
        super::HostName::Herdr
    }

    fn sessions<'a>(&'a self, _state: &'a Path) -> BoxFuture<'a, Result<Vec<super::Session>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn close<'a>(
        &'a self,
        _state: &'a Path,
        place: &'a super::Origin,
        _owner: &'a super::Owner,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            anyhow::ensure!(
                place.owned,
                "Shoal did not create Herdr pane {}",
                place.id()
            );
            close_pane(&place.data, place.id()).await
        })
    }
}

#[cfg(test)]
mod tests;
