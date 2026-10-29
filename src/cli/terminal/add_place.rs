//! The place a host opened for an addition: the add worker that runs there,
//! and how the place hosts the workspace it adds.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result};

use super::{
    closing,
    here::{label_current, place_here, record_owned_in},
    host::{Host, Places},
    hosts::{Configured, named},
    sessions::Owner,
};
use crate::{
    cli::{commands::workspaces::execute_add, context::Context},
    env,
    model::Workspace,
    sessions::HostName,
};

/// The place a host opened for this process to add a workspace in, hosting
/// it in place of the caller's shell.
pub(in crate::cli) struct Place {
    host: &'static dyn Host,
    /// The place's ID in its origin record.
    id: String,
    closes_when_done: bool,
}

/// The worker that adds the workspace `plan` describes in the place `host`
/// opened for it, closing the place with the workspace when config says so.
/// The plan reaches it in the place's environment.
pub(super) fn add_worker_argv(ctx: &Context, host: &Configured) -> Result<Vec<OsString>> {
    crate::cli::internal::internal_command(
        &ctx.paths,
        false,
        crate::cli::internal::Worker::Add {
            host: host.host.name(),
            close_when_done: host.closes_when_done,
        },
    )
}

/// The add worker: add the workspace `payload` plans in the place of the
/// host named `host` this process runs in.
pub(in crate::cli) async fn add_worker(
    mut ctx: Context,
    host: HostName,
    closes_when_done: bool,
    payload: &str,
) -> Result<i32> {
    let plan = serde_json::from_str(payload).context("invalid workspace plan")?;
    let host = named(host);
    let id = place_here(host)
        .with_context(|| format!("{} did not name the place this worker runs in", host.name()))?;
    ctx.place = Some(Place {
        host,
        id,
        closes_when_done,
    });
    execute_add(&ctx, plan).await
}

/// Tell the place this process runs in that it hosts `workspace`, whose
/// agent starts there when `runs_agent`: it takes `label`, and closes with the
/// workspace when it should. A session the user opened is labelled once an
/// agent starts in it.
pub(in crate::cli) async fn host_workspace(
    ctx: &Context,
    workspace: &Workspace,
    runs_agent: bool,
    label: &str,
) -> Result<()> {
    match (&ctx.place, runs_agent) {
        // A place is labelled before it is watched, since the watcher only
        // closes a place it finds hosting the workspace.
        (Some(place), _) => {
            if let Some(sessions) = place.host.sessions() {
                sessions
                    .label_current(&ctx.paths.state, Owner::Workspace(&workspace.id))
                    .await?;
            }
            if let Some(places) = place.host.places() {
                places.rename_place(&place.id, label).await;
            }
            record_owned_in(ctx, place.host, &workspace.id).await;
            if place.closes_when_done {
                closing::start_watcher(ctx, place.host, &place.id, &workspace.id).await?;
            }
            Ok(())
        }
        (None, true) => label_current(&ctx.paths, Owner::Workspace(&workspace.id)).await,
        (None, false) => Ok(()),
    }
}

/// Whether the shell that started this process enters a new workspace
/// afterwards: the caller's own or that of the place opened for it.
pub(in crate::cli) fn follows_caller(ctx: &Context) -> bool {
    ctx.place
        .as_ref()
        .is_none_or(|place| place.host.places().is_none_or(Places::places_keep_shells))
}

/// Give a workspace added without an agent a shell at `cwd` in the place
/// opened for it, which stays open while the shell runs. The caller's own
/// terminal gets its shell back instead.
pub(in crate::cli) async fn open_shell(ctx: &Context, cwd: &Path) -> Result<i32> {
    if ctx.place.is_none() {
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
    Ok(env::session_exit_code(crate::execution::exit_code(status)))
}
