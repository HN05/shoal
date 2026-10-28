//! Durable ownership of panes in the Herdr server that created them.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fsutil::{self, Permissions, ReplaceOptions};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct Place {
    pub socket: String,
    pub pane: String,
    pub terminal: String,
    pub workspace: String,
    pub demo: Option<String>,
}

impl Place {
    pub fn current_server(pane: &str, workspace: Option<&str>, demo: Option<&str>) -> Result<Self> {
        let socket = std::env::var("HERDR_SOCKET_PATH")
            .context("Herdr did not provide HERDR_SOCKET_PATH")?;
        ensure!(!socket.is_empty(), "Herdr socket path is empty");
        Ok(Self {
            socket,
            pane: pane.into(),
            terminal: String::new(),
            workspace: workspace.unwrap_or_default().into(),
            demo: demo.map(Into::into),
        })
    }

    pub fn key(&self) -> String {
        Sha256::digest(format!("{}\0{}", self.socket, self.pane).as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn owned_by(&self, owner: &crate::sessions::Owner) -> bool {
        match owner {
            crate::sessions::Owner::Workspace(id) => self.workspace == *id,
            crate::sessions::Owner::Demo(id) => self.demo.as_ref() == Some(id),
        }
    }
}

fn directory(state: &Path) -> PathBuf {
    state.join("terminal-sessions/herdr")
}

/// Serialize relabelling and closing across CLI and daemon processes. Waiting
/// for another process's lock must never block the async executor.
pub(super) async fn lock(state: &Path) -> Result<fs::File> {
    let directory = directory(state);
    tokio::task::spawn_blocking(move || {
        fs::create_dir_all(&directory)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("ownership.lock"))?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    })
    .await?
}

fn path(state: &Path, key: &str) -> Result<PathBuf> {
    ensure!(
        key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid Herdr ownership key"
    );
    Ok(directory(state).join(format!("{key}.json")))
}

pub(super) fn read(state: &Path, key: &str) -> Result<Option<Place>> {
    match fs::read(path(state, key)?) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn owned(
    state: &Path,
    session: &str,
    owner: &crate::sessions::Owner,
) -> Result<Option<Place>> {
    let place = match owner {
        crate::sessions::Owner::Workspace(_) => read(state, session)?,
        // Demo handles name panes for screen/input; launch IDs locate the
        // server-qualified record for cleanup.
        crate::sessions::Owner::Demo(id) => list(state)?
            .into_iter()
            .find(|place| place.demo.as_ref() == Some(id) && place.pane == session),
    };
    Ok(place.filter(|place| place.owned_by(owner)))
}

pub(super) fn write(state: &Path, place: &Place) -> Result<()> {
    fsutil::replace_atomically(
        &path(state, &place.key())?,
        &serde_json::to_vec(place)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(())
}

pub(super) fn forget(state: &Path, key: &str) -> Result<()> {
    match fs::remove_file(path(state, key)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn list(state: &Path) -> Result<Vec<Place>> {
    let entries = match fs::read_dir(directory(state)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut places = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            places.push(serde_json::from_slice(&fs::read(path)?)?);
        }
    }
    Ok(places)
}
