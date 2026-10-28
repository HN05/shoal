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
    Run::new(kill)
        .checked()
        .await
        .with_context(|| format!("end zmx session {session}"))?;
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

#[cfg(test)]
mod tests;

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
