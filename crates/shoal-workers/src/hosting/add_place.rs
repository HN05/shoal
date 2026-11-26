//! The place a host opened for a recorded space.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::path::Path;

use anyhow::{Context as _, Result};

use super::{
    Caller,
    here::{created_here, label_current, place_here, record_owned},
};
use crate::{
    env,
    sessions::HostName,
    terminal::{Host, Owner, Places, named},
};

/// The place a host opened for this process to run a space's agent or a
/// workspace's shell in, hosting the space in place of the caller's shell.
pub struct Place {
    host: &'static dyn Host,
    /// The place's ID in its origin record.
    id: String,
    closes_when_done: bool,
    /// Whether the host opened the place focused, showing it.
    focused: bool,
}

impl Place {
    /// The place of `host` this process runs in, which it opened for it,
    /// `focused` or not, closing with the workspace when `closes_when_done`.
    pub fn here(host: HostName, closes_when_done: bool, focused: bool) -> Result<Self> {
        let host = named(host);
        let id = place_here(host).with_context(|| {
            format!("{} did not name the place this worker runs in", host.name())
        })?;
        Ok(Self {
            host,
            id,
            closes_when_done,
            focused,
        })
    }
}

/// Tell the place this process runs in that it hosts the space of `owner`,
/// whose agent starts there when `runs_agent`: it takes `label`, closes
/// with a workspace when it should, and once opened focused is where
/// `attach -` returns. A session the user opened is labelled once an agent
/// starts in it.
pub async fn host_space(
    caller: Caller<'_>,
    owner: Owner<'_>,
    runs_agent: bool,
    label: &str,
) -> Result<()> {
    match (caller.place, runs_agent) {
        // A place is labelled before it is watched, since the watcher only
        // closes a place it finds hosting the workspace.
        (Some(place), _) => {
            if let Some(sessions) = place.host.sessions() {
                sessions.label_current(&caller.paths.state, owner).await?;
            }
            label_place(caller, place.host, owner.id(), label).await;
            if place.closes_when_done
                && let Owner::Workspace(workspace) = owner
            {
                let (paths, json) = (caller.paths, caller.json);
                crate::watchers::start_watcher(paths, json, place.host, &place.id, workspace)
                    .await?;
            }
            if place.focused {
                crate::history::record_attach(caller.paths, &SpaceSelector::id(owner.id())).await;
            }
            Ok(())
        }
        (None, true) => label_current(caller.paths, owner).await,
        (None, false) => Ok(()),
    }
}

/// Show `label` on the place of `host` this process runs in, one `host`
/// created for `space`, and record it as that space's. Neither is needed
/// for the space to work, so a failure only warns.
async fn label_place(caller: Caller<'_>, host: &dyn Host, space: &SpaceId, label: &str) {
    let Some(here) = created_here(host).await else {
        return;
    };
    if let Some(places) = host.places()
        && let Err(error) = places.rename_place(&here, None, label).await
    {
        eprintln!("warning: cannot name the {} place: {error:#}", host.name());
    }
    record_owned(caller, space, here).await;
}

/// Whether the shell that started this process enters a new workspace
/// afterwards: the caller's own or that of the place opened for it.
pub fn follows_caller(caller: Caller<'_>) -> bool {
    caller
        .place
        .as_ref()
        .is_none_or(|place| place.host.places().is_none_or(Places::places_keep_shells))
}

/// Give a workspace added without an agent a shell at `cwd` in the place
/// opened for it, which stays open while the shell runs. The caller's own
/// terminal gets its shell back instead.
pub async fn open_shell(caller: Caller<'_>, cwd: &Path) -> Result<i32> {
    if caller.place.is_none() {
        return Ok(0);
    }
    let shell = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let status = tokio::process::Command::new(shell)
        .arg("-i")
        .current_dir(cwd)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .context("open workspace shell")?;
    Ok(env::session_exit_code(shoal_core::execution::exit_code(
        status,
    )))
}
