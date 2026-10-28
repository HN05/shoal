//! zmx session discovery and termination, shared by launchers and cleanup.
use anyhow::{Context as _, Result};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::process::Command;

use crate::{subprocess::Run, tools::Tool};

pub(crate) const WORKSPACE_LABEL: &str = "shoal.workspace";

/// End `session` with everything running in it.
pub(crate) async fn kill(session: &str) -> Result<()> {
    let mut kill = Command::new(Tool::Zmx.program());
    kill.args(["kill", session]);
    let result = Run::new(kill).checked().await;
    // The session's shell or a place watcher may have closed it first.
    if result.is_err()
        && list()
            .await
            .is_ok_and(|sessions| !sessions.iter().any(|(name, _)| name == session))
    {
        return Ok(());
    }
    result.with_context(|| format!("end zmx session {session}"))?;
    Ok(())
}

pub(crate) type Fields = Vec<(String, String)>;

/// Every zmx session with its fields and labels.
pub(crate) async fn list() -> Result<Vec<(String, Fields)>> {
    let mut command = Command::new(Tool::Zmx.program());
    command.arg("list");
    let output = Run::new(command)
        .checked()
        .await
        .context("list zmx sessions")?;
    Ok(parse_list(&String::from_utf8_lossy(&output.stdout)))
}

/// `zmx list` prints one session per line after a two-column marker, as
/// tab-separated `key=value` fields followed by its labels.
pub(crate) fn parse_list(output: &str) -> Vec<(String, Fields)> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Fields = line[line.find("name=")?..]
                .split('\t')
                .filter_map(|field| field.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect();
            let name = field(&fields, "name")?.to_owned();
            Some((name, fields))
        })
        .collect()
}

pub(crate) fn field<'a>(fields: &'a Fields, key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// A whitespace-free state-directory identity for zmx's space-separated labels.
pub(crate) fn state_label(state: &Path) -> String {
    format!(
        "shoal.state={}",
        Sha256::digest(state.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

/// Unnamespaced sessions predate state labels and belong to the default daemon.
fn in_state(fields: &Fields, state: &Path, include_legacy: bool) -> bool {
    match field(fields, "shoal.state") {
        Some(label) => state_label(state) == format!("shoal.state={label}"),
        None => include_legacy,
    }
}

pub(super) struct Zmx;

impl super::Host for Zmx {
    fn name(&self) -> &'static str {
        "zmx"
    }
    fn sessions<'a>(
        &'a self,
        state: &'a Path,
        include_legacy: bool,
    ) -> futures_util::future::BoxFuture<'a, Result<Vec<super::Session>>> {
        Box::pin(async move {
            Ok(list()
                .await?
                .into_iter()
                .filter(|(_, fields)| in_state(fields, state, include_legacy))
                .filter_map(|(name, fields)| {
                    let workspace = field(&fields, WORKSPACE_LABEL)?.to_owned();
                    Some(super::Session { name, workspace })
                })
                .collect())
        })
    }

    fn close<'a>(&'a self, session: &'a str) -> futures_util::future::BoxFuture<'a, Result<()>> {
        Box::pin(kill(session))
    }
}

#[cfg(test)]
mod tests;
