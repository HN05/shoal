//! zmx session discovery and termination, shared by launchers and cleanup.
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::process::Command;

use crate::{subprocess::Run, tools::Tool};

pub(crate) const WORKSPACE_LABEL: &str = "shoal.workspace";
pub(crate) const CHAT_LABEL: &str = "shoal.chat";
/// Set only when Shoal creates a session. Relabelling never adds it, so a
/// session the user opened stays theirs after Shoal runs an agent in it.
pub(crate) const CREATED: &str = "shoal.created=1";
/// The oldest zmx Shoal supports. Since 1.0.0 a zmx reaches only servers of
/// its own version, and 1.0.0's macOS release could not reach its own.
const OLDEST: (u32, u32, u32) = (1, 0, 1);

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
    reported_socket_directory(&version().await?)
}

/// Refuse a zmx older than [`OLDEST`].
pub(crate) async fn ensure_supported() -> Result<()> {
    let version = version().await?;
    if let Some(found) = parse_version(&version) {
        ensure!(
            found >= OLDEST,
            "zmx {} is too old; Shoal needs zmx {}.{}.{} or later",
            reported(&version, "zmx").unwrap_or_default(),
            OLDEST.0,
            OLDEST.1,
            OLDEST.2
        );
    }
    Ok(())
}

/// What `zmx version` prints: one tab-separated value per line.
async fn version() -> Result<String> {
    let mut command = Command::new(Tool::Zmx.program());
    command.arg("version");
    let output = Run::new(command).checked().await?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn reported_socket_directory(version: &str) -> Result<std::path::PathBuf> {
    reported(version, "socket_dir")
        .map(Into::into)
        .context("zmx did not report its socket directory")
}

/// The version `zmx version` reports, such as 1.0.1.
fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = reported(version, "zmx")?.split('.').map(|part| {
        let digits = part
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(part.len());
        part[..digits].parse().ok()
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The value `zmx version` reports for `key`, when it reports one.
fn reported<'a>(version: &'a str, key: &str) -> Option<&'a str> {
    version
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find_map(|(name, value)| {
            (name == key)
                .then(|| value.trim_start_matches('\t'))
                .filter(|value| !value.is_empty())
        })
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

/// The fields of the session `name` among `sessions`, while zmx lists it.
pub(crate) fn find<'a>(sessions: &'a [(String, Fields)], name: &str) -> Result<Option<&'a Fields>> {
    sessions
        .iter()
        .find(|(listed, _)| listed == name)
        .map(|(listed, fields)| reachable(listed, fields))
        .transpose()
}

/// Refuse `sessions` while one of them, whatever its owner, is unreachable.
fn ensure_reachable(sessions: &[(String, Fields)]) -> Result<()> {
    sessions
        .iter()
        .try_for_each(|(name, fields)| reachable(name, fields).map(drop))
}

/// zmx lists a session it cannot talk to, such as one a zmx of another
/// version started, by name and error alone. Without its labels nothing tells
/// whose it is, so it is refused rather than taken for another owner's.
fn reachable<'a>(name: &str, fields: &'a Fields) -> Result<&'a Fields> {
    if field(fields, "status") == Some("unreachable") {
        bail!(
            "zmx cannot reach session {name} ({}), which a zmx of another version may have \
             started; end its processes and restart the agents that ran there",
            field(fields, "err").unwrap_or("no error reported")
        );
    }
    Ok(fields)
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

/// Sessions this state directory currently owns and that Shoal created.
/// Older sessions carry neither label, so their origin is unknown.
fn in_state(fields: &Fields, state_label: &str) -> bool {
    created(fields)
        && field(fields, "shoal.state")
            .is_some_and(|label| state_label == format!("shoal.state={label}"))
}

fn created(fields: &Fields) -> bool {
    CREATED
        .split_once('=')
        .is_some_and(|(key, value)| field(fields, key) == Some(value))
}

pub(super) struct Zmx;

impl super::Host for Zmx {
    fn name(&self) -> super::HostName {
        super::HostName::Zmx
    }

    fn ensure_installed(&self) -> Result<()> {
        anyhow::ensure!(Tool::Zmx.installed(), "zmx is not installed or not on PATH");
        Ok(())
    }
    fn sessions<'a>(
        &'a self,
        state: &'a Path,
    ) -> futures_util::future::BoxFuture<'a, Result<Vec<super::Session>>> {
        Box::pin(async move {
            // Without zmx there are no sessions to find. A close still runs
            // for a session recorded as zmx's, and fails until zmx is back,
            // since its server can outlive the executable.
            if !Tool::Zmx.installed() {
                return Ok(Vec::new());
            }
            let state_label = state_label(state)?;
            let sessions = list().await?;
            ensure_reachable(&sessions)?;
            Ok(sessions
                .into_iter()
                .filter(|(_, fields)| in_state(fields, &state_label))
                .filter_map(|(name, fields)| {
                    let space = SpaceId::from(space_owner(&fields)?.to_owned());
                    Some(super::Session { name, space })
                })
                .collect())
        })
    }

    fn close<'a>(
        &'a self,
        _state: &'a Path,
        place: &'a super::Origin,
        owner: &'a super::Owner,
    ) -> futures_util::future::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let session = place.id();
            let _server = ownership_lock().await?;
            if find(&list().await?, session)?.is_some_and(|fields| owned_by(fields, owner)) {
                kill(session).await?;
            }
            Ok(())
        })
    }
}

/// Session names can be reused; a close must still own the current session,
/// which Shoal created.
fn owned_by(fields: &Fields, owner: &super::Owner) -> bool {
    if !created(fields) {
        return false;
    }
    match owner {
        super::Owner::Space(id) => space_owner(fields) == Some(id.as_str()),
        super::Owner::Demo(id) => {
            field(fields, CHAT_LABEL).is_none() && field(fields, "shoal.demo") == Some(id.as_str())
        }
    }
}

/// The space that owns a session: a chat that took over a workspace's
/// session labels it as its own, and old launchers left the workspace's label
/// beside the chat's.
fn space_owner(fields: &Fields) -> Option<&str> {
    field(fields, CHAT_LABEL).or_else(|| field(fields, WORKSPACE_LABEL))
}

#[cfg(test)]
mod tests;
