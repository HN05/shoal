//! The space's machine's side of `shoal attach` from another machine or a
//! device.
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
    terminal::{self, Elsewhere, Session},
};

/// Attach this terminal, which `viewer` views from another machine, to a
/// session of `space` as `shoal attach` would on `machine`, this one, and
/// report to the descriptor `report` the session it chose, then whether
/// that session still runs once the viewer left it. A session that ends as
/// its agent moves to a new place gives way to the agent's next, reported
/// as chosen in turn. Attaching again after a lost connection finds the
/// session at the place it names, without waking or restoring its agent a
/// second time.
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
    let steps = Steps { paths, attaching };
    let (mut target, mut session) = match &viewer.again {
        Some(place) => super::sessions::session_again(paths, target, place).await?,
        None => (steps.awake_session(target, viewer.session.clone(), Some(&machine))).await?,
    };
    let open = loop {
        if show_chosen(&mut report, &target, &session, &viewer, &machine).await? {
            break true;
        }
        match steps.moved_session(target).await? {
            Some(moved) => (target, session) = moved,
            None => break false,
        }
    };
    send_report(&mut report, &AttachReport::Left { open })?;
    Ok(0)
}

/// Report `session` of `target` as chosen on `report`, then show it to
/// `viewer` from `machine`, this one, until they leave it; whether it still
/// runs then.
async fn show_chosen(
    report: &mut std::fs::File,
    target: &Target,
    session: &Session,
    viewer: &Viewer,
    machine: &str,
) -> Result<bool> {
    let chosen = ChosenSession {
        name: session.name.clone(),
        place: session.place.clone(),
    };
    send_report(report, &AttachReport::Chosen(chosen))?;
    let shown = Elsewhere {
        machine,
        app: viewer.terminal.app(session.place.host),
    };
    terminal::show_elsewhere(&target.number().to_string(), session, &shown).await
}

fn send_report(report: &mut std::fs::File, line: &AttachReport) -> Result<()> {
    writeln!(report, "{}", serde_json::to_string(line)?).context("report to the daemon")
}
