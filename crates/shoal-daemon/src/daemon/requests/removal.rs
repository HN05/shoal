//! The daemon's answers to [`RemovalMethod`] requests, and who may send them.
use shoal_protocol::model::ByKind;
use std::sync::Arc;

use anyhow::Result;

use tokio::net::UnixStream;

use super::super::{
    answer::{self, Answer},
    cleanup,
    manager::Manager,
    restore::PackageInput,
    scope::{Caller, Scoped},
    spaces::MovePacking,
};
use crate::{
    model::StoppedSpace,
    process::identity,
    protocol::{self, Body, QueuedLaunches, RemovalMethod, Response, UntrackedProcesses},
    removal::InspectionPolicy,
    transfer::{PackageImport, PackageSource},
};
use shoal_protocol::processes::Selection;
use shoal_protocol::space_names::SpaceSelector;

/// Whether `method` moves a space through a lifecycle state that an exit
/// would interrupt, which an exit that waits for work in progress holds back.
fn changes_lifecycle(method: &RemovalMethod) -> bool {
    match method {
        RemovalMethod::StopSpace { .. }
        | RemovalMethod::RemoveSpace { .. }
        | RemovalMethod::StartRemoval { .. }
        | RemovalMethod::AbsorbSpace(_)
        | RemovalMethod::RestoreSpace { .. }
        | RemovalMethod::ExportMovingSpace { .. }
        | RemovalMethod::ImportPackage(_)
        | RemovalMethod::ReceiveMove { .. } => true,
        RemovalMethod::Cleanup { dry_run, .. } => !dry_run,
        RemovalMethod::StopTargets
        | RemovalMethod::CheckStop { .. }
        | RemovalMethod::StopProcesses { .. }
        | RemovalMethod::CheckRemoval { .. }
        | RemovalMethod::RemovedSpaces
        | RemovalMethod::RemovedSpace { .. }
        | RemovalMethod::DropRestoredSessions { .. }
        | RemovalMethod::ExportRemovedSpace { .. } => false,
        // A move in progress finishes, which an exit that waits for it
        // waits for.
        RemovalMethod::EndMovedSpace { .. }
        | RemovalMethod::AbandonMove { .. }
        | RemovalMethod::DeliverMove { .. }
        | RemovalMethod::DeliverCopy { .. }
        | RemovalMethod::SettleMove { .. } => false,
    }
}

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: RemovalMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    if changes_lifecycle(&method) {
        manager.gates.admit()?;
    }
    Ok(Answer::Body(match method {
        RemovalMethod::RemovedSpaces => {
            let listed = manager.restorable_spaces(caller).await;
            return Ok(Answer::pages(listed, Body::RemovedSpaces));
        }
        RemovalMethod::RemovedSpace { space } => {
            Body::RemovedSpaces(vec![manager.removed_space(&space).await?])
        }
        RemovalMethod::StopTargets => {
            let targets = manager.stop_targets(QueuedLaunches::Cancel).await?;
            Body::Spaces(manager.spaces_below(caller, targets).await?)
        }
        RemovalMethod::StopSpace {
            space,
            acknowledge_unproven,
            launches,
            processes,
        } => {
            let stop = SpaceStop {
                acknowledge_unproven,
                launches,
                processes,
            };
            Body::StoppedSpace(stop_space(manager, &space, stop).await?)
        }
        RemovalMethod::CheckStop { space } => {
            // A chat has no lifecycle state that refuses a stop.
            if let ByKind::Workspace(workspace) = manager.space(&space).await?.by_kind() {
                manager.check_stop(&workspace).await?
            }
            Body::Ok
        }
        RemovalMethod::StopProcesses {
            space,
            selection,
            requester,
        } => Body::StoppedProcesses(
            manager
                .stop_space_processes(&space, selection, identity_of(requester))
                .await?,
        ),
        RemovalMethod::CheckRemoval {
            space,
            include_changes,
        } => Body::RemovalCheck(Box::new(
            manager
                .check_manual_removal(&space, include_changes)
                .await?,
        )),
        RemovalMethod::RemoveSpace {
            space,
            unsaved,
            keep_user_holds,
            above,
        } => {
            let inspection = InspectionPolicy::GitOnly;
            let releases = !keep_user_holds && releases_holds(manager, caller, &space).await?;
            Body::RemovalResult(match (above, releases) {
                (Some(cause), _) => {
                    let confirmed = unsaved.children();
                    (manager.remove_as_child(&space, (cause, !releases), confirmed)).await?
                }
                (None, true) => manager.remove_selected(&space, unsaved, inspection).await?,
                (None, false) => manager.remove_unheld(&space, unsaved, inspection).await?,
            })
        }
        RemovalMethod::StartRemoval { space, unsaved } => {
            let release_holds = releases_holds(manager, caller, &space).await?;
            (manager.start_removal(&space, unsaved, caller, release_holds)).await?;
            Body::Ok
        }
        RemovalMethod::AbsorbSpace(absorb) => {
            Body::Absorbed(Box::new(manager.absorb_space(&absorb, caller).await?))
        }
        RemovalMethod::Cleanup { dry_run, selection } => {
            Body::Cleanup(cleanup::on_request(manager, dry_run, &selection).await?)
        }
        RemovalMethod::RestoreSpace { space } => {
            Body::RestoredSpace(manager.restore_space(&space, caller).await?)
        }
        RemovalMethod::DropRestoredSessions { space } => {
            manager.drop_restored_sessions(&space).await?;
            Body::Ok
        }
        RemovalMethod::ExportRemovedSpace {
            space,
            file,
            carrying,
        } => Body::ExportedSpace(
            manager
                .export_removed_space(&space, file, &carrying)
                .await?,
        ),
        RemovalMethod::ExportMovingSpace {
            space,
            unsaved,
            destination,
            carrying,
            again,
        } => {
            let packing = MovePacking {
                unsaved,
                destination,
                carrying,
                again,
            };
            Body::ExportedSpace(manager.export_moving_space(&space, packing).await?)
        }
        RemovalMethod::EndMovedSpace { space } => {
            Body::RemovalResult(manager.end_moved_space(&space).await?)
        }
        RemovalMethod::AbandonMove { space } => {
            Body::MoveAbandoned(manager.abandon_move(&space).await?)
        }
        RemovalMethod::DeliverMove { space } => {
            Body::ImportedMove(manager.deliver_move(&space).await?)
        }
        RemovalMethod::DeliverCopy { space, destination } => {
            Body::ImportedMove(manager.deliver_copy(&space, &destination).await?)
        }
        RemovalMethod::SettleMove { space } => {
            Body::MoveSettled(manager.settle_move(&space).await?)
        }
        RemovalMethod::ImportPackage(import) => {
            return import_package(manager, *import, caller).await;
        }
        RemovalMethod::ReceiveMove { ticket, resume } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                manager.receive_move(stream, request_id, (ticket, resume), guard)
            }));
        }
    }))
}

/// Import the package `import` names: from its file, or from the request's
/// connection once the first reply asks for it.
async fn import_package(
    manager: &Arc<Manager>,
    import: PackageImport,
    caller: Option<&Caller>,
) -> Result<Answer> {
    let PackageSource::File(file) = &import.package else {
        let manager = manager.clone();
        let caller = caller.cloned();
        return Ok(Answer::connection(
            move |stream, request_id, guard| async move {
                let _guard = guard;
                let caller = caller.as_ref();
                import_streamed(&manager, &import, caller, stream, request_id).await
            },
        ));
    };
    let input = PackageInput::File(file.clone());
    let imported = manager.import_package(&import, input, caller).await?;
    Ok(Answer::Body(Body::ImportedPackage(imported)))
}

/// Ask the client on `stream` for the package `import` streams, then answer
/// `request_id` with how its import went.
async fn import_streamed(
    manager: &Manager,
    import: &PackageImport,
    caller: Option<&Caller>,
    mut stream: UnixStream,
    request_id: u64,
) -> Result<()> {
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    let replying = stream.into_std()?;
    let input = PackageInput::Connection(replying.try_clone()?);
    let imported = manager.import_package(import, input, caller).await;
    replying.set_nonblocking(true)?;
    let mut stream = UnixStream::from_std(replying)?;
    let body = match imported {
        Ok(imported) => Body::ImportedPackage(imported),
        Err(error) => answer::failure(&error),
    };
    answer::reply(&mut stream, request_id, body).await
}

/// Whether removing `space` at `caller`'s request releases the user's holds
/// of it and of the spaces below it: the user's own request always does,
/// and a scoped caller's once its `user_holds` permission lets it take the
/// holds there are. A hold the user places after that still refuses the
/// removal.
async fn releases_holds(
    manager: &Manager,
    caller: Option<&Caller>,
    space: &SpaceSelector,
) -> Result<bool> {
    match caller {
        Some(caller) => manager.authorize_held_removal(caller, space, true).await,
        None => Ok(true),
    }
}

/// What a [`RemovalMethod::StopSpace`] request asks of the stop.
struct SpaceStop {
    acknowledge_unproven: bool,
    launches: QueuedLaunches,
    processes: UntrackedProcesses,
}

/// Stop the agents and commands of `space`, then its untracked processes
/// as `stop` says, reporting whether the stop saved work for `shoal
/// restore`; a CI job's workspace has its run cancelled instead.
async fn stop_space(
    manager: &Manager,
    space: &SpaceSelector,
    stop: SpaceStop,
) -> Result<StoppedSpace> {
    let mut stopped = manager
        .stop_space_manually(space, stop.acknowledge_unproven, stop.launches)
        .await?;
    if stopped.cancelled_run.is_some() {
        return Ok(stopped);
    }
    if let UntrackedProcesses::Stop { requester } = stop.processes {
        let untracked = manager
            .stop_space_processes(space, Selection::Processes, identity_of(requester))
            .await?;
        stopped.other_processes = untracked.processes;
    }
    // A read that fails reports nothing saved rather than failing the stop.
    stopped.saved = (manager.saved_work(space).await).is_ok_and(|saved| !saved.is_empty());
    Ok(stopped)
}

/// The identity of the requesting process `pid`, which a stop of untracked
/// processes spares with the processes that started it.
fn identity_of(pid: Option<u32>) -> Option<identity::Identity> {
    pid.and_then(|pid| identity::capture(pid).ok().flatten())
}

impl Scoped for RemovalMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        const REFUSAL: &str = "scoped processes remove only the spaces below their own; finish their own with shoal done";
        match self {
            // The check asks for the user's holds too, so a batch refuses
            // before removing any.
            RemovalMethod::CheckRemoval { space, .. } => {
                let space = manager.workspace_below(caller, space, REFUSAL).await?;
                manager.authorize_held_removal(caller, space, false).await?;
                Ok(Some(space))
            }
            // The removal takes the user's holds where it releases them.
            RemovalMethod::RemoveSpace { space, .. }
            | RemovalMethod::StartRemoval { space, .. } => manager
                .workspace_below(caller, space, REFUSAL)
                .await
                .map(Some),
            // Into a space its scope reaches, the user's holds taken where
            // it may release them.
            RemovalMethod::AbsorbSpace(absorb) => {
                manager
                    .ensure_selected_reached(caller, absorb.into.as_mut())
                    .await?;
                const ABSORB: &str = "scoped processes absorb only the spaces below their own";
                (manager.workspace_below(caller, &mut absorb.space, ABSORB))
                    .await
                    .map(Some)
            }
            // Its answer confines the spaces to those below the caller's.
            RemovalMethod::StopTargets => Ok(None),
            RemovalMethod::StopSpace { space, .. } => manager
                .workspace_below(
                    caller,
                    space,
                    "scoped processes stop only the spaces below theirs",
                )
                .await
                .map(Some),
            // Its own processes too: a scoped caller's shells and their
            // commands descend from its execution, which keeps them.
            RemovalMethod::StopProcesses { space, .. } => Ok(Some(space)),
            RemovalMethod::CheckStop { space } => Ok(Some(space)),
            // Its answer lists only the spaces the caller may restore.
            RemovalMethod::RemovedSpaces => Ok(None),
            // Below its recorded parent, within the caller's scope.
            RemovalMethod::RestoreSpace { space } | RemovalMethod::RemovedSpace { space } => {
                manager.ensure_restorable_by(space, caller).await?;
                Ok(None)
            }
            // Cleaning up and moving spaces is the `admin` permission's.
            RemovalMethod::Cleanup { .. }
            | RemovalMethod::DropRestoredSessions { .. }
            | RemovalMethod::ExportRemovedSpace { .. }
            | RemovalMethod::ExportMovingSpace { .. }
            | RemovalMethod::EndMovedSpace { .. }
            | RemovalMethod::AbandonMove { .. }
            | RemovalMethod::DeliverMove { .. }
            | RemovalMethod::DeliverCopy { .. }
            | RemovalMethod::SettleMove { .. }
            | RemovalMethod::ImportPackage(_)
            | RemovalMethod::ReceiveMove { .. } => administer(manager, caller, self).await,
        }
    }
}

/// Let `caller` send `method`, which the `admin` permission governs, in its
/// own repository where it imports into one, returning the existing space it
/// acts on, which its scope must reach before it asks approval.
async fn administer<'a>(
    manager: &Manager,
    caller: &Caller,
    method: &'a mut RemovalMethod,
) -> Result<Option<&'a mut SpaceSelector>> {
    let importing = match method {
        RemovalMethod::ImportPackage(import) => import.repository.as_mut(),
        _ => None,
    };
    let repository = match importing {
        Some(repository) => {
            const REFUSAL: &str = "scoped processes import only into their own repository";
            *repository = manager.own_repository(caller, repository, REFUSAL).await?;
            Some(manager.repository(&*repository).await?.id)
        }
        None => None,
    };
    manager
        .ensure_selected_reached(caller, administered(method))
        .await?;
    let repository = match (administered(method), repository) {
        (Some(space), None) => manager.space_repository(space).await?,
        (_, repository) => repository,
    };
    manager.administer(caller, &*method, repository).await?;
    Ok(administered(method))
}

/// The existing space `method`, which the `admin` permission governs, acts
/// on: one it moves, or a restored workspace whose kept sessions it drops.
fn administered(method: &mut RemovalMethod) -> Option<&mut SpaceSelector> {
    match method {
        RemovalMethod::ExportMovingSpace { space, .. }
        | RemovalMethod::EndMovedSpace { space }
        | RemovalMethod::AbandonMove { space }
        | RemovalMethod::DeliverMove { space }
        | RemovalMethod::DeliverCopy { space, .. }
        | RemovalMethod::SettleMove { space }
        | RemovalMethod::DropRestoredSessions { space } => Some(space),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
