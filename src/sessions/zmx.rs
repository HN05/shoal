//! zmx session discovery and termination, shared by launchers and cleanup.
use anyhow::{Context as _, Result};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::process::Command;

use crate::{subprocess::Run, tools::Tool};

pub(crate) const WORKSPACE_LABEL: &str = "shoal.workspace";
pub(crate) const CHAT_LABEL: &str = "shoal.chat";

/// Serialize in-place ownership replacement with verified cleanup in the native
/// namespace, regardless of which Shoal state directory initiated it.
pub(crate) async fn ownership_lock() -> Result<std::fs::File> {
    let directory = socket_directory().await?;
    super::locking::for_server(&directory).await
}

async fn socket_directory() -> Result<std::path::PathBuf> {
    if let Some(directory) = std::env::var_os("ZMX_DIR").filter(|value| !value.is_empty()) {
        return Ok(directory.into());
    }
    let mut command = Command::new(Tool::Zmx.program());
    command.arg("version");
    let output = Run::new(command).checked().await?;
    reported_socket_directory(&String::from_utf8_lossy(&output.stdout))
}

fn reported_socket_directory(version: &str) -> Result<std::path::PathBuf> {
    version
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find_map(|(key, value)| {
            (key == "socket_dir")
                .then(|| value.trim_start_matches('\t'))
                .filter(|value| !value.is_empty())
        })
        .map(Into::into)
        .context("zmx did not report its socket directory")
}

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

/// A whitespace-free canonical directory identity for zmx's labels.
pub(crate) fn state_label(state: &Path) -> Result<String> {
    let state = state
        .canonicalize()
        .context("resolve session state directory")?;
    Ok(format!(
        "shoal.state={}",
        Sha256::digest(state.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

/// Unnamespaced sessions predate state labels and belong to the default daemon.
fn in_state(fields: &Fields, state_label: &str, include_legacy: bool) -> bool {
    match field(fields, "shoal.state") {
        Some(label) => state_label == format!("shoal.state={label}"),
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
            let state_label = state_label(state)?;
            Ok(list()
                .await?
                .into_iter()
                .filter(|(_, fields)| in_state(fields, &state_label, include_legacy))
                .filter_map(|(name, fields)| {
                    let workspace = workspace_owner(&fields)?.to_owned();
                    Some(super::Session { name, workspace })
                })
                .collect())
        })
    }

    fn close<'a>(
        &'a self,
        _state: &'a Path,
        session: &'a str,
        owner: &'a super::Owner,
    ) -> futures_util::future::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let _server = ownership_lock().await?;
            if list()
                .await?
                .iter()
                .any(|(name, fields)| name == session && owned_by(fields, owner))
            {
                kill(session).await?;
            }
            Ok(())
        })
    }
}

/// Session names can be reused; a close must still own the current session.
fn owned_by(fields: &Fields, owner: &super::Owner) -> bool {
    match owner {
        super::Owner::Workspace(id) => workspace_owner(fields) == Some(id.as_str()),
        super::Owner::Demo(id) => {
            field(fields, CHAT_LABEL).is_none() && field(fields, "shoal.demo") == Some(id.as_str())
        }
    }
}

/// Old launchers could leave both owner labels when a workspace session became
/// a chat. Conflicting ownership must never authorize workspace cleanup.
fn workspace_owner(fields: &Fields) -> Option<&str> {
    if field(fields, CHAT_LABEL).is_some() {
        return None;
    }
    field(fields, WORKSPACE_LABEL)
}

#[cfg(test)]
mod tests;
