//! Moving one space, with the spaces it carries: the daemon records the
//! move and packs them, stopping their agents; the package goes to its
//! destination; the spaces end here only once the destination has it, so
//! a failed move leaves them here. When the destination has nothing of
//! them, the agents the move stopped restore here; when how its import
//! ended is unknown, they stay stopped, since the spaces may continue
//! there.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::{
    Steps,
    checks::Checked,
    ssh::{ImportFailure, Remote},
};
use crate::{
    client::{connection::CommandPaths, output::batch::Failure},
    model::Space,
    protocol::{ErrorCode, RemoteError, operations::Step},
    resume::{Resuming, queue::queue_each},
};
use shoal_core::execution::recovery::{SavedCommand, SavedSession};
use shoal_core::transfer::{
    Arrived, Carrying, ExportedSpace, Manifest, MoveDestination, MoveLock, MoveOrder, SpaceMoved,
    SshDestination, StoppedExecutions,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::removal::{RemovalResult, UnsavedWork};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// One space on its move: where its package goes on the way, which of the
/// spaces below it go with it, and what its end here may delete.
pub(super) struct Moving<'a> {
    pub(super) id: SpaceId,
    pub(super) name: SpaceName,
    pub(super) destination: &'a MoveDestination,
    pub(super) carrying: Carrying,
    pub(super) unsaved: UnsavedWork,
    pub(super) file: PathBuf,
    /// Whether it packs again under its move's record, as the move's
    /// finisher does.
    pub(super) again: bool,
}

/// Move the `checked` space, with the spaces it carries, as `order` says,
/// its package going to `file` on the way, telling `steps` how far it got,
/// while it holds the move's lock. A package for its command's standard
/// output is the command's to copy, under the lock it holds, before it
/// ends the space.
pub(super) async fn move_one(
    paths: &CommandPaths,
    order: &MoveOrder,
    (checked, file): (Checked, PathBuf),
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    let moving = Moving {
        id: checked.id().clone(),
        name: checked.name().clone(),
        destination: &order.destination,
        carrying: order.carrying(),
        unsaved: checked.unsaved(),
        file,
        again: false,
    };
    if let MoveDestination::Client { .. } = moving.destination {
        let exported = pack(paths, &moving, steps).await?;
        return Ok(moving.packed(exported));
    }
    let lock = MoveLock::take(paths, &moving.id).await?;
    let moved = move_locked(paths, &moving, steps).await;
    lock.release();
    moved
}

/// Pack the `moving` space, deliver its package and end it here.
pub(super) async fn move_locked(
    paths: &CommandPaths,
    moving: &Moving<'_>,
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    let exported = pack(paths, moving, steps).await?;
    let mut moved = deliver_and_end(paths, moving, Some(exported.manifest), steps).await?;
    moved.warnings = exported.warnings;
    Ok(moved)
}

/// Have the daemon record the move of the `moving` space, stop its agents
/// and those of the spaces that go with it, and write their package; a
/// failure leaves them here.
async fn pack(
    paths: &CommandPaths,
    moving: &Moving<'_>,
    steps: &impl Steps,
) -> Result<ExportedSpace> {
    steps.reached(Step::MovePacking).await;
    let space = SpaceSelector::id(&moving.id);
    let (unsaved, carrying) = (moving.unsaved.clone(), moving.carrying.clone());
    let destination = moving.destination.clone();
    let again = moving.again;
    let packing =
        crate::client::transfer::export_moving(paths, space, unsaved, destination, carrying, again);
    match packing.await {
        Ok(exported) => Ok(exported),
        // That move is another's to finish.
        Err(error) if refused_as(&error, ErrorCode::MoveInProgress) => Err(error),
        Err(error) => Err(stays_here(paths, &moving.id, error).await),
    }
}

/// Deliver the package of the `moving` space, which `manifest` describes
/// where known, and end the space here once its destination has it.
pub(super) async fn deliver_and_end(
    paths: &CommandPaths,
    moving: &Moving<'_>,
    manifest: Option<Manifest>,
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    let arrived = deliver(paths, moving.destination, (&moving.id, &moving.file), steps).await?;
    let removal = end(paths, moving, steps).await?;
    Ok(moving.moved(manifest, removal, arrived))
}

impl Moving<'_> {
    /// The report of this space, packed as `exported`, which has not ended
    /// here.
    fn packed(&self, exported: ExportedSpace) -> SpaceMoved {
        SpaceMoved {
            id: self.id.clone(),
            name: self.name.clone(),
            manifest: Some(exported.manifest),
            file: Some(self.file.clone()),
            removal: None,
            arrived: None,
            warnings: exported.warnings,
        }
    }

    /// The report of this space, whose package `manifest` describes where
    /// known, once it ended here with `removal`, where it `arrived` on
    /// another machine.
    pub(super) fn moved(
        &self,
        manifest: Option<Manifest>,
        removal: RemovalResult,
        arrived: Option<Arrived>,
    ) -> SpaceMoved {
        let shipped = self.destination.machine().is_some();
        SpaceMoved {
            id: self.id.clone(),
            name: self.name.clone(),
            manifest,
            file: (!shipped).then(|| self.file.clone()),
            removal: Some(removal),
            arrived,
            warnings: Vec::new(),
        }
    }
}

/// End the `moving` space here, which its destination has.
pub(super) async fn end(
    paths: &CommandPaths,
    moving: &Moving<'_>,
    steps: &impl Steps,
) -> Result<RemovalResult> {
    steps.reached(Step::MoveEnding).await;
    crate::client::transfer::end_moved(paths, SpaceSelector::id(&moving.id))
        .await
        .with_context(|| both_places(moving.destination, &moving.file, &moving.id))
}

/// Hand the package of the space `id` in `file` to `destination`: import
/// it on another machine, which reports where it arrived where it can; a
/// file already holds it.
async fn deliver(
    paths: &CommandPaths,
    destination: &MoveDestination,
    (id, file): (&SpaceId, &PathBuf),
    steps: &impl Steps,
) -> Result<Option<Arrived>> {
    match destination {
        MoveDestination::Ssh(destination) => {
            steps.reached(Step::MoveDelivering).await;
            import_over_ssh(paths, destination, (id, file))
                .await
                .map(Some)
        }
        MoveDestination::Machine(destination) => {
            steps.reached(Step::MoveDelivering).await;
            super::joined::deliver(paths, id, destination, steps).await
        }
        MoveDestination::File { .. }
        | MoveDestination::Directory { .. }
        | MoveDestination::Client { .. } => Ok(None),
    }
}

/// Import the package of the space `id` in `file` on the machine `ssh`
/// reaches at `destination`, which reports where it arrived.
async fn import_over_ssh(
    paths: &CommandPaths,
    destination: &SshDestination,
    (id, file): (&SpaceId, &PathBuf),
) -> Result<Arrived> {
    let remote = Remote::new(destination);
    match remote.import(file).await {
        Ok(arrived) => Ok(arrived),
        Err(ImportFailure::Refused(error)) => Err(stays_here(paths, id, error).await),
        Err(ImportFailure::Unknown(error)) => {
            Err(maybe_there(paths, id, remote.host(), error).await)
        }
    }
}

/// Whether the daemon refused `error`'s request with `code`.
fn refused_as(error: &anyhow::Error, code: ErrorCode) -> bool {
    error
        .downcast_ref::<RemoteError>()
        .is_some_and(|refusal| refusal.code == code)
}

/// `error`, which left it unknown whether `holder`, another machine or
/// what read its package, has the space `id`, once its move is forgotten
/// and its agents stay stopped here, since the space may continue there.
pub(super) async fn maybe_there(
    paths: &CommandPaths,
    id: &SpaceId,
    holder: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    let space = SpaceSelector::id(id);
    if let Err(abandoned) = crate::client::transfer::abandon_move(paths, space).await {
        super::warn(&format!("warning: {abandoned:#}"));
    }
    error.context(format!(
        "{holder} may already have it, so its agents stay stopped here; if it arrived there, shoal rm {id} removes it here, and otherwise shoal resume {id} continues it here"
    ))
}

/// What a move to `destination` of the space `id`, whose package went to
/// `file`, says when the space could not end here after all.
fn both_places(destination: &MoveDestination, file: &Path, id: &SpaceId) -> String {
    let there = match super::packages::other_machine(destination) {
        Some(machine) => format!("on {machine}"),
        None => format!("in {}", file.display()),
    };
    format!(
        "it now exists here and {there}; once the work you want is there, shoal rm {id} removes it here"
    )
}

/// `error`, which kept the space `id` here, once its move is forgotten and
/// the agents the move stopped restore here.
pub(super) async fn stays_here(
    paths: &CommandPaths,
    id: &SpaceId,
    error: anyhow::Error,
) -> anyhow::Error {
    match crate::client::transfer::abandon_move(paths, SpaceSelector::id(id)).await {
        Ok(stopped) => restore_here(paths, stopped).await,
        Err(abandoned) => super::warn(&format!("warning: {abandoned:#}")),
    }
    error.context("it stays here")
}

/// Queue the restores of the agents among `stopped`, warning when that
/// fails, since `shoal resume` still resumes them.
async fn restore_here(paths: &CommandPaths, stopped: StoppedExecutions) {
    let mut restoring = Vec::new();
    for (space, executions) in stopped {
        let selector = SpaceSelector::id(&space);
        let present = crate::client::workspaces::present_space(paths, &selector).await;
        let Ok(Some(space)) = present else {
            continue;
        };
        let Ok(mut saved) = crate::client::saved::read(paths, selector).await else {
            super::warn("warning: cannot read the stopped agents; run shoal resume");
            continue;
        };
        saved.retain(|id| executions.contains(id));
        if !(saved.agents.is_empty() && saved.sessions.is_empty()) {
            restoring.push((space, saved));
        }
    }
    queue_each(paths, restoring, false, &RestoringHere).await;
}

/// How a move restores here the agents it stopped: without a terminal to
/// pick in, telling of what fails in its output.
struct RestoringHere;

impl Resuming for RestoringHere {
    fn pick_agent(&self, _agents: &[SavedSession], _labels: &[String]) -> Result<SavedSession> {
        anyhow::bail!("multiple stopped agents; run shoal resume to pick one")
    }

    fn stopped_commands(&self, space: &Space, commands: &[SavedCommand]) {
        if !commands.is_empty() {
            let message = format!(
                "warning: {} has {} stopped commands; shoal resume lists them",
                space.name,
                commands.len()
            );
            super::warn(&message);
        }
    }

    fn failed(&self, space: &str, error: &anyhow::Error) -> Failure {
        super::warn(&format!(
            "warning: cannot resume {space}; run shoal resume: {error:#}"
        ));
        Failure {
            workspace: space.to_owned(),
            error: format!("{error:#}"),
        }
    }
}

#[cfg(test)]
mod tests;
