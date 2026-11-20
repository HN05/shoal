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
    protocol::operations::Step,
    resume::{Resuming, queue::queue_each},
};
use shoal_core::execution::recovery::{SavedCommand, SavedSession};
use shoal_core::transfer::{Arrived, MoveDestination, MoveOrder, SpaceMoved, StoppedExecutions};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// Move the `checked` space, with the spaces it carries, as `order` says,
/// its package going to `file` on the way, telling `steps` how far it got.
pub(super) async fn move_one(
    paths: &CommandPaths,
    order: &MoveOrder,
    (checked, file): (Checked, PathBuf),
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    let id = checked.id().clone();
    let unsaved = (order.spaces.iter())
        .find(|ordered| ordered.space == id)
        .map_or_else(|| checked.unsaved(), |ordered| ordered.unsaved.clone());
    steps.reached(Step::MovePacking).await;
    let space = SpaceSelector::id(&id);
    let carrying = order.carrying();
    let destination = order.destination.clone();
    let packing =
        crate::client::transfer::export_moving(paths, space, unsaved, destination, carrying);
    let exported = match packing.await {
        Ok(exported) => exported,
        Err(error) => return Err(stays_here(paths, &id, error).await),
    };
    let arrived = deliver(paths, &order.destination, (&id, &file), steps).await?;
    steps.reached(Step::MoveEnding).await;
    let removal = crate::client::transfer::end_moved(paths, SpaceSelector::id(&id))
        .await
        .with_context(|| both_places(&order.destination, &file, &id))?;
    Ok(SpaceMoved {
        id,
        name: checked.name().clone(),
        manifest: exported.manifest,
        file: (!matches!(order.destination, MoveDestination::Ssh(_))).then_some(file),
        removal: Some(removal),
        arrived,
        warnings: exported.warnings,
    })
}

/// Hand the package of the space `id` in `file` to `destination`: import
/// it on another machine, which reports where it arrived; a file already
/// holds it.
async fn deliver(
    paths: &CommandPaths,
    destination: &MoveDestination,
    (id, file): (&SpaceId, &PathBuf),
    steps: &impl Steps,
) -> Result<Option<Arrived>> {
    let MoveDestination::Ssh(destination) = destination else {
        return Ok(None);
    };
    steps.reached(Step::MoveDelivering).await;
    let remote = Remote::new(destination);
    match remote.import(file).await {
        Ok(arrived) => Ok(Some(arrived)),
        Err(ImportFailure::Refused(error)) => Err(stays_here(paths, id, error).await),
        Err(ImportFailure::Unknown(error)) => {
            let space = SpaceSelector::id(id);
            if let Err(abandoned) = crate::client::transfer::abandon_move(paths, space).await {
                super::warn(&format!("warning: {abandoned:#}"));
            }
            Err(error.context(format!(
                "{host} may already have it, so its agents stay stopped here; if it arrived there, shoal rm {id} removes it here, and otherwise shoal resume {id} continues it here",
                host = remote.host(),
            )))
        }
    }
}

/// What a move to `destination` of the space `id`, whose package went to
/// `file`, says when the space could not end here after all.
fn both_places(destination: &MoveDestination, file: &Path, id: &SpaceId) -> String {
    let there = match destination {
        MoveDestination::Ssh(destination) => format!("on {}", destination.host),
        _ => format!("in {}", file.display()),
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
