//! Finishing a move whose worker went before it ended, from the phase its
//! record reached.
use anyhow::{Result, anyhow};

use super::{
    Steps,
    moving::{self, Moving},
};
use crate::client::connection::CommandPaths;
use shoal_core::transfer::{
    Manifest, MoveDestination, MoveFailure, MoveLock, MovePhase, MoveRecord, MoveReport, SpaceMoved,
};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// Finish the move `record` records, unless its worker still runs it,
/// telling `steps` how far it got.
pub async fn finish(
    paths: &CommandPaths,
    record: MoveRecord,
    steps: &impl Steps,
) -> Result<MoveReport> {
    let mut report = MoveReport::default();
    let Some(lock) = MoveLock::try_take(paths, &record.space)? else {
        return Ok(report);
    };
    let name = space_name(paths, &record).await;
    let moving = Moving {
        id: record.space.clone(),
        name: name.clone(),
        destination: &record.destination,
        carrying: record.carrying.clone(),
        unsaved: record.unsaved.clone(),
        file: (record.destination).package(&record.space, &paths.move_packages()),
        again: true,
    };
    let finished = finish_locked(paths, &record, &moving, steps).await;
    lock.release();
    match finished {
        Ok(moved) => report.moved.push(moved),
        Err(error) => report.failed.push(MoveFailure {
            id: record.space.clone(),
            name,
            error: format!("{error:#}"),
        }),
    }
    Ok(report)
}

/// Finish the `moving` space's move from the phase its `record` reached.
async fn finish_locked(
    paths: &CommandPaths,
    record: &MoveRecord,
    moving: &Moving<'_>,
    steps: &impl Steps,
) -> Result<SpaceMoved> {
    let packed = package_manifest(moving);
    match (record.phase, moving.destination) {
        (MovePhase::Delivering, MoveDestination::Ssh(destination)) => {
            let error = anyhow!("the move stopped while {} imported it", destination.host);
            Err(moving::maybe_there(paths, &moving.id, &destination.host, error).await)
        }
        (MovePhase::Delivering, MoveDestination::Machine(destination)) => {
            let machine = format!("machine {}", destination.machine);
            let error = anyhow!("the move stopped while {machine} imported it");
            super::joined::settle(paths, &moving.id, &machine, error, steps).await?;
            let removal = moving::end(paths, moving, steps).await?;
            Ok(moving.moved(packed, removal, None))
        }
        (MovePhase::Delivering, MoveDestination::Client { .. } | MoveDestination::Held) => {
            let error = anyhow!("the command that took its package went before it ended it");
            let holder = "what read its package";
            Err(moving::maybe_there(paths, &moving.id, holder, error).await)
        }
        (MovePhase::Delivering | MovePhase::Delivered, _) => {
            let removal = moving::end(paths, moving, steps).await?;
            Ok(moving.moved(packed, removal, None))
        }
        (MovePhase::Packing, MoveDestination::Client { .. } | MoveDestination::Held) => {
            let error = anyhow!("the command that would have taken its package went");
            Err(moving::stays_here(paths, &moving.id, error).await)
        }
        (MovePhase::Packing, _) => {
            // A package written before its phase was recorded goes, so the
            // record holds the checkpoints of the one written again.
            if packed.is_some() {
                std::fs::remove_file(&moving.file)?;
            }
            moving::move_locked(paths, moving, steps).await
        }
    }
}

/// What the package of the `moving` space holds, when its file is that
/// space's package.
fn package_manifest(moving: &Moving<'_>) -> Option<Manifest> {
    let file = std::fs::File::open(&moving.file).ok()?;
    let manifest = shoal_core::transfer::read_manifest(&mut std::io::BufReader::new(file)).ok()?;
    (manifest.space.record.space_id == moving.id).then_some(manifest)
}

/// The name of the space `record` moves, or its ID once it is gone.
async fn space_name(paths: &CommandPaths, record: &MoveRecord) -> SpaceName {
    let selector = SpaceSelector::id(&record.space);
    match crate::client::spaces::present_space(paths, &selector).await {
        Ok(Some(space)) => space.name,
        _ => record.space.to_string().into(),
    }
}

#[cfg(test)]
mod tests;
