//! The place this process runs in: naming it, recording it as a
//! workspace's, and closing it with the workspace.
use anyhow::{Context as _, Result};

use super::{
    closing,
    host::{Host, Places},
    hosts::{hosts, named},
    sessions::Owner,
};
use crate::{
    cli::context::Context,
    sessions::{HostName, Origin},
};

/// The place this process runs in, as the host serving it names it.
pub(crate) fn current_origin() -> Option<Origin> {
    hosts().find_map(origin_here)
}

/// The place of `host` this process runs in, as `host` names it, with what
/// a later start needs to open a place beside it.
pub(super) fn origin_here(host: &dyn Host) -> Option<Origin> {
    host.places().and_then(Places::origin)
}

/// The place this process runs in, as [`current_origin`] names it, with what
/// tells it apart from a later place under its ID, for the daemon's record of
/// where an execution or chat runs. Without that, a record left by a process
/// that crashed would name a later place under the ID. A host that cannot
/// tell leaves it out, with a warning.
pub(crate) async fn current_place() -> Option<Origin> {
    for host in hosts() {
        if let Some(origin) = origin_here(host) {
            return Some(match with_instance(host, origin.clone()).await {
                Ok(place) => place,
                Err(error) => {
                    eprintln!(
                        "warning: cannot tell this {} place apart: {error:#}",
                        host.name()
                    );
                    origin
                }
            });
        }
    }
    None
}

/// `place` of `host` with what tells it apart from a later place under its
/// ID, for a host that reuses IDs.
async fn with_instance(host: &dyn Host, mut place: Origin) -> Result<Origin> {
    if let Some(watches) = host.watches() {
        place.instance = watches.place_instance(place.id()).await?;
    }
    Ok(place)
}

/// Whether the place this process runs in closes once `workspace` is
/// removed, ending anything it streams: a watcher closes it then, or the
/// daemon does, which records it as a place Shoal created for the workspace
/// or whose host labels it as the workspace's session.
pub(in crate::cli) async fn closes_with(
    paths: &crate::paths::Paths,
    workspace: &str,
) -> Result<bool> {
    if closing::closes_with(paths, workspace).await {
        return Ok(true);
    }
    match current_place().await {
        Some(place) => Ok(
            crate::cli::client::place_ownership(paths, workspace, &place).await?
                == crate::sessions::PlaceOwnership::Workspace,
        ),
        None => Ok(false),
    }
}

/// Record `place`, one Shoal created for `workspace`, so the daemon closes
/// it once the workspace is removed. Without the record the place stays
/// open, so a failure only warns.
async fn record_owned(ctx: &Context, workspace: &str, place: Origin) {
    if let Err(error) = crate::cli::client::record_owned_place(&ctx.paths, workspace, place).await {
        eprintln!("warning: cannot record the place Shoal opened: {error:#}");
    }
}

/// Record the place this process runs in, one the host named `host`
/// created for `workspace`, as [`record_owned`] does.
pub(in crate::cli) async fn record_owned_here(ctx: &Context, host: HostName, workspace: &str) {
    record_owned_in(ctx, named(host), workspace).await;
}

/// Record the place of `host` this process runs in, one `host` created for
/// `workspace`, as [`record_owned`] does. Only that host names it: another
/// host's variables this process inherited name a place Shoal did not
/// create. A host that reuses IDs tells this place apart from an earlier one
/// under its ID, whose record it replaces.
pub(super) async fn record_owned_in(ctx: &Context, host: &dyn Host, workspace: &str) {
    let Some(place) = origin_here(host) else {
        return;
    };
    match with_instance(host, place).await {
        Ok(place) => record_owned(ctx, workspace, place).await,
        Err(error) => eprintln!("warning: cannot record the place Shoal opened: {error:#}"),
    }
}

/// Close the place of the host named `host` that this process runs in once
/// `workspace` is done. Without a watcher the place stays open, so a
/// failure only warns.
pub(in crate::cli) async fn close_here_when_done(ctx: &Context, host: HostName, workspace: &str) {
    if let Err(error) = watch_here(ctx, host, workspace, false).await {
        eprintln!("warning: {error:#}");
    }
}

/// Close the place of the host named `host` that this process runs in, one
/// opened in the background for `workspace` before anything labelled it,
/// once the workspace is done. The place is labelled with the workspace
/// first, since the watcher only closes a place it finds hosting it; the
/// agent that starts there adds its own name.
pub(in crate::cli) async fn close_background_place_when_done(
    ctx: &Context,
    host: HostName,
    workspace: &str,
) {
    if let Err(error) = watch_here(ctx, host, workspace, true).await {
        eprintln!("warning: {error:#}");
    }
}

/// Watch the place of the host named `host` that this process runs in,
/// labelling it with `workspace` first when `label`.
async fn watch_here(ctx: &Context, host: HostName, workspace: &str, label: bool) -> Result<()> {
    let host = named(host);
    let place = place_here(host)
        .with_context(|| format!("{} did not name the place this agent runs in", host.name()))?;
    if label && let Some(sessions) = host.sessions() {
        sessions
            .label_current(&ctx.paths.state, Owner::Workspace(workspace))
            .await?;
    }
    closing::start_watcher(ctx, host, &place, workspace).await
}

/// The ID of the place of `host` this process runs in, from its origin record.
pub(super) fn place_here(host: &dyn Host) -> Option<String> {
    origin_here(host).and_then(|origin| origin.place)
}

/// Mark the session this process runs in, when it runs in one, as
/// `owner`'s, so cleanup closes it with `owner`.
pub(in crate::cli) async fn label_current(
    paths: &crate::paths::Paths,
    owner: Owner<'_>,
) -> Result<()> {
    for host in hosts().filter_map(Host::sessions) {
        host.label_current(&paths.state, owner).await?;
    }
    Ok(())
}
