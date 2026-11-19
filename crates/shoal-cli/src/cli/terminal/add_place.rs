//! The place a host opened for a recorded space: the place worker that
//! runs there, and how the place hosts the space.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::path::Path;

use anyhow::{Context as _, Result};

use super::{
    closing,
    here::{created_here, label_current, place_here, record_owned},
};
use crate::{
    cli::{args::internal::PlaceWorkerArgs, commands::execute_place, context::Context, recent},
    env,
    sessions::HostName,
    terminal::{Host, Owner, Places, named},
};

/// The place a host opened for this process to run a space's agent or a
/// workspace's shell in, hosting the space in place of the caller's shell.
pub(in crate::cli) struct Place {
    host: &'static dyn Host,
    /// The place's ID in its origin record.
    id: String,
    closes_when_done: bool,
    /// Whether the host opened the place focused, showing it.
    focused: bool,
}

/// The place worker: carry out the plan `args` give in the place of the
/// host they name that this process runs in, or in no place without one.
pub(in crate::cli) async fn place_worker(mut ctx: Context, args: PlaceWorkerArgs) -> Result<i32> {
    let plan = serde_json::from_str(&args.plan).context("invalid place plan")?;
    if let Some(host) = args.host {
        ctx.place = Some(Place::here(host, args.close_when_done, args.focused)?);
    }
    execute_place(&ctx, plan).await
}

impl Place {
    /// The place of `host` this process runs in, which it opened for it,
    /// `focused` or not, closing with the workspace when `closes_when_done`.
    fn here(host: HostName, closes_when_done: bool, focused: bool) -> Result<Self> {
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
pub(in crate::cli) async fn host_space(
    ctx: &Context,
    owner: Owner<'_>,
    runs_agent: bool,
    label: &str,
) -> Result<()> {
    match (&ctx.place, runs_agent) {
        // A place is labelled before it is watched, since the watcher only
        // closes a place it finds hosting the workspace.
        (Some(place), _) => {
            if let Some(sessions) = place.host.sessions() {
                sessions.label_current(&ctx.paths.state, owner).await?;
            }
            label_place(ctx, place.host, owner.id(), label).await;
            if place.closes_when_done
                && let Owner::Workspace(workspace) = owner
            {
                closing::start_watcher(ctx, place.host, &place.id, workspace).await?;
            }
            if place.focused {
                recent::record_attach(ctx, &SpaceSelector::id(owner.id())).await;
            }
            Ok(())
        }
        (None, true) => label_current(&ctx.paths, owner).await,
        (None, false) => Ok(()),
    }
}

/// Show `label` on the place of `host` this process runs in, one `host`
/// created for `space`, and record it as that space's. Neither is needed
/// for the space to work, so a failure only warns.
async fn label_place(ctx: &Context, host: &dyn Host, space: &SpaceId, label: &str) {
    let Some(here) = created_here(host).await else {
        return;
    };
    if let Some(places) = host.places()
        && let Err(error) = places.rename_place(&here, None, label).await
    {
        eprintln!("warning: cannot name the {} place: {error:#}", host.name());
    }
    record_owned(ctx, space, here).await;
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
