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
use shoal_protocol::attaching::{AttachReport, ChosenSession, Viewer};

use super::{Attaching, Target, sessions::Steps};
use crate::{
    client::connection::CommandPaths,
    terminal::{self, Elsewhere},
};

/// Attach this terminal, which `viewer` views from another machine, to a
/// session of `space` as `shoal attach` would on `machine`, this one, and
/// report to the descriptor `report` the session it chose, then whether
/// that session still runs once the viewer left it. Attaching again
/// after a lost connection finds the session at the place it names,
/// without waking or restoring its agent a second time.
pub async fn for_viewer(
    paths: &CommandPaths,
    space: SpaceId,
    viewer: Viewer,
    machine: String,
    report: i32,
    attaching: &impl Attaching,
) -> Result<i32> {
    // SAFETY: the daemon opened `report` for this process alone.
    let mut report = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(report) });
    let target = Target::by_id(paths, space).await?;
    super::report_attach(paths, &target).await?;
    let (target, session) = match &viewer.again {
        Some(place) => super::sessions::session_again(paths, target, place).await?,
        None => {
            let steps = Steps { paths, attaching };
            (steps.awake_session(target, viewer.session.clone(), Some(&machine))).await?
        }
    };
    let chosen = ChosenSession {
        name: session.name.clone(),
        place: session.place.clone(),
    };
    send_report(&mut report, &AttachReport::Chosen(chosen))?;
    let shown = Elsewhere {
        machine: &machine,
        app: viewer.app(session.place.host),
    };
    let open = terminal::show_elsewhere(&target.number().to_string(), &session, &shown).await?;
    send_report(&mut report, &AttachReport::Left { open })?;
    Ok(0)
}

/// Write `line` to `report` for the daemon.
fn send_report(report: &mut std::fs::File, line: &AttachReport) -> Result<()> {
    writeln!(report, "{}", serde_json::to_string(line)?).context("report to the daemon")
}
