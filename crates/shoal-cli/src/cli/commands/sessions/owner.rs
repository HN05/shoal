//! The space's machine's side of `shoal attach` from another machine or a
//! device: the daemon runs it on a terminal it made for that user here, and
//! it takes the steps a local attach takes, then shows the session as its
//! host shows a place to another machine.
use std::{
    io::Write as _,
    os::fd::{FromRawFd as _, OwnedFd},
};

use anyhow::{Context as _, Result};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    attaching::{ChosenSession, Viewer},
    space_names::SpaceSelector,
};

use super::Target;
use crate::{
    cli::context::Context,
    terminal::{self, Elsewhere},
};

/// Attach this terminal, which `viewer` views from another machine, to a
/// session of `space` as `shoal attach` would on `machine`, this one, and
/// write the session it chose to the descriptor `report`. Attaching again
/// after a lost connection finds the session at the place it names,
/// without waking or restoring its agent a second time.
pub(in crate::cli::commands) async fn attach_for_viewer(
    ctx: &Context,
    space: SpaceId,
    viewer: Viewer,
    machine: String,
    report: i32,
) -> Result<i32> {
    // SAFETY: the daemon opened `report` for this process alone.
    let mut report = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(report) });
    let target = Target::by_id(ctx, space).await?;
    // Attaching is the user's interaction with the space's agent.
    crate::client::agents::report_attach(&ctx.paths, SpaceSelector::id(target.id())).await?;
    let (target, session) = match &viewer.again {
        Some(place) => target.session_again(ctx, place).await?,
        None => (target.awake_session(ctx, viewer.session.clone(), Some(&machine))).await?,
    };
    let chosen = ChosenSession {
        name: session.name.clone(),
        place: session.place.clone(),
    };
    writeln!(report, "{}", serde_json::to_string(&chosen)?).context("report the session")?;
    drop(report);
    let shown = Elsewhere {
        machine: &machine,
        app: viewer.app(session.place.host),
    };
    terminal::show_elsewhere(&target.number().to_string(), &session, &shown).await?;
    Ok(0)
}
