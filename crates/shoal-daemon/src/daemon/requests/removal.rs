//! The daemon's answers to [`RemovalMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use shoal_protocol::model::ByKind;
use std::sync::Arc;

use anyhow::Result;

use tokio::net::UnixStream;

use super::super::{
    answer::{self, Answer},
    cleanup,
    hub_moves::Receiving,
    manager::Manager,
    restore::PackageInput,
    scope::{Caller, Scoped},
    spaces::MovePacking,
    target_checks::{Checked, TargetCheck},
};
use crate::{
    model::StoppedSpace,
    process::identity,
    protocol::{self, Body, QueuedLaunches, RemovalMethod, Response, UntrackedProcesses},
    removal::InspectionPolicy,
    transfer::{MoveDestination, PackageImport, PackageSource},
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
        | RemovalMethod::LocateRemovedSpace { .. }
        | RemovalMethod::DropRestoredSessions { .. }
        | RemovalMethod::ExportRemovedSpace { .. }
        | RemovalMethod::CopySpace { .. }
        | RemovalMethod::ReadHeldPackage { .. }
        | RemovalMethod::ReleaseHeldMove { .. } => false,
        // A move in progress finishes, which an exit that waits for it
        // waits for.
        RemovalMethod::EndMovedSpace { .. }
        | RemovalMethod::AbandonMove { .. }
        | RemovalMethod::DeliverMove { .. }
        | RemovalMethod::DeliverCopy { .. }
        | RemovalMethod::SettleMove { .. }
        | RemovalMethod::TakenRecords { .. }
        | RemovalMethod::DeliverRemovedSpace { .. }
        | RemovalMethod::SettleRemovedMove { .. } => false,
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
            let listed = manager.spaces.restorable_spaces(&**manager, caller).await;
            return Ok(Answer::pages(listed, Body::RemovedSpaces));
        }
        RemovalMethod::RemovedSpace { space } => Body::RemovedSpaces(vec![
            manager.spaces.restorable_space(&**manager, &space).await?,
        ]),
        RemovalMethod::LocateRemovedSpace { space } => Body::LocatedRemoved(Box::new(
            manager
                .spaces
                .locate_removed(&**manager, &space, caller)
                .await?,
        )),
        RemovalMethod::StopTargets => {
            let targets = manager
                .agents
                .stop_targets(&**manager, QueuedLaunches::Cancel)
                .await?;
            Body::Spaces(
                manager
                    .coordination
                    .spaces_below(&**manager, caller, targets)
                    .await?,
            )
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
            if let ByKind::Workspace(workspace) = manager.spaces.space(&space).await?.by_kind() {
                manager.agents.check_stop(&**manager, &workspace).await?
            }
            Body::Ok
        }
        RemovalMethod::StopProcesses {
            space,
            selection,
            requester,
        } => Body::StoppedProcesses(
            manager
                .spaces
                .stop_space_processes(&**manager, &space, selection, identity_of(requester))
                .await?,
        ),
        RemovalMethod::CheckRemoval {
            space,
            include_changes,
        } => Body::RemovalCheck(Box::new(
            manager
                .spaces
                .check_manual_removal(&**manager, &space, include_changes)
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
                    (manager.spaces.remove_as_child(
                        &**manager,
                        &space,
                        (cause, !releases),
                        confirmed,
                    ))
                    .await?
                }
                (None, true) => {
                    manager
                        .spaces
                        .remove_selected(&**manager, &space, unsaved, inspection)
                        .await?
                }
                (None, false) => {
                    manager
                        .spaces
                        .remove_unheld(&**manager, &space, unsaved, inspection)
                        .await?
                }
            })
        }
        RemovalMethod::StartRemoval { space, unsaved } => {
            let release_holds = releases_holds(manager, caller, &space).await?;
            (manager.spaces.start_removal(
                &manager.daemon(),
                &space,
                unsaved,
                caller,
                release_holds,
            ))
            .await?;
            Body::Ok
        }
        RemovalMethod::AbsorbSpace(absorb) => Body::Absorbed(Box::new(
            manager
                .spaces
                .absorb_space(&manager.daemon(), &absorb, caller)
                .await?,
        )),
        RemovalMethod::Cleanup { dry_run, selection } => {
            Body::Cleanup(cleanup::on_request(&**manager, dry_run, &selection).await?)
        }
        RemovalMethod::RestoreSpace { space } => Body::RestoredSpace(
            manager
                .spaces
                .restore_space(&**manager, &space, caller)
                .await?,
        ),
        RemovalMethod::DropRestoredSessions { space } => {
            manager
                .spaces
                .drop_restored_sessions(&**manager, &space)
                .await?;
            Body::Ok
        }
        RemovalMethod::ExportRemovedSpace {
            space,
            file,
            carrying,
        } => Body::ExportedSpace(
            manager
                .spaces
                .export_removed_space(&**manager, &space, file, &carrying)
                .await?,
        ),
        RemovalMethod::CopySpace {
            space,
            checkpoint,
            file,
            carrying,
        } => Body::ExportedSpace(
            manager
                .spaces
                .copy_space(&**manager, &space, checkpoint, file, &carrying)
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
            Body::ExportedSpace(
                manager
                    .spaces
                    .export_moving_space(&**manager, &space, packing)
                    .await?,
            )
        }
        RemovalMethod::EndMovedSpace { space } => {
            Body::RemovalResult(manager.spaces.end_moved_space(&**manager, &space).await?)
        }
        RemovalMethod::AbandonMove { space } => {
            Body::MoveAbandoned(manager.spaces.abandon_move(&**manager, &space).await?)
        }
        RemovalMethod::DeliverMove { space } => {
            Body::ImportedMove(manager.spaces.deliver_move(&**manager, &space).await?)
        }
        RemovalMethod::DeliverCopy { space, destination } => Body::ImportedMove(
            manager
                .spaces
                .deliver_copy(&**manager, &space, &destination)
                .await?,
        ),
        RemovalMethod::SettleMove { space } => {
            Body::MoveSettled(manager.spaces.settle_move(&**manager, &space).await?)
        }
        RemovalMethod::TakenRecords { space, carrying } => {
            Body::RemovedSpaces(manager.spaces.taken_records(&space, &carrying).await?)
        }
        RemovalMethod::DeliverRemovedSpace {
            space,
            destination,
            carrying,
            ticket,
        } => Body::RemovedDelivered(
            manager
                .spaces
                .deliver_removed(&**manager, &space, &destination, &carrying, &ticket)
                .await?,
        ),
        RemovalMethod::SettleRemovedMove {
            space,
            carried,
            machine,
            ticket,
        } => Body::MoveSettled(
            manager
                .spaces
                .settle_removed_move(&**manager, &space, &carried, &machine, &ticket)
                .await?,
        ),
        RemovalMethod::ReleaseHeldMove { space } => {
            manager.spaces.release_held_move(&**manager, &space).await?;
            Body::Ok
        }
        RemovalMethod::ReadHeldPackage { space, copy } => {
            let package = manager.spaces.held_package(&space, copy).await?;
            let manager = manager.clone();
            return Ok(Answer::connection(
                move |stream, request_id, guard| async move {
                    manager
                        .spaces
                        .send_held_package(stream, request_id, package, guard)
                        .await
                },
            ));
        }
        RemovalMethod::ImportPackage(import) => {
            return import_package(manager, *import, caller).await;
        }
        RemovalMethod::ReceiveMove {
            ticket,
            resume,
            from,
            removed,
        } => {
            let manager = manager.clone();
            let receiving = Receiving {
                ticket,
                resume,
                from,
                removed,
            };
            return Ok(Answer::connection(
                move |stream, request_id, guard| async move {
                    manager
                        .spaces
                        .receive_move(&*manager, stream, request_id, receiving, guard)
                        .await
                },
            ));
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
    let imported = manager
        .spaces
        .import_package(&**manager, &import, input, caller)
        .await?;
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
    let imported = manager
        .spaces
        .import_package(manager, import, input, caller)
        .await;
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
        Some(caller) => {
            manager
                .coordination
                .authorize_held_removal(manager, caller, space, true)
                .await
        }
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
        .agents
        .stop_space_manually(manager, space, stop.acknowledge_unproven, stop.launches)
        .await?;
    if stopped.cancelled_run.is_some() {
        return Ok(stopped);
    }
    if let UntrackedProcesses::Stop { requester } = stop.processes {
        let untracked = manager
            .spaces
            .stop_space_processes(manager, space, Selection::Processes, identity_of(requester))
            .await?;
        stopped.other_processes = untracked.processes;
    }
    // A read that fails reports nothing saved rather than failing the stop.
    stopped.saved =
        (manager.agents.saved_work(manager, space).await).is_ok_and(|saved| !saved.is_empty());
    Ok(stopped)
}

/// The identity of the requesting process `pid`, which a stop of untracked
/// processes spares with the processes that started it.
fn identity_of(pid: Option<u32>) -> Option<identity::Identity> {
    pid.and_then(|pid| identity::capture(pid).ok().flatten())
}

impl Checked for RemovalMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::CheckStop { .. }
            | Self::CheckRemoval { .. }
            | Self::RemoveSpace { .. }
            | Self::StartRemoval { .. }
            | Self::AbsorbSpace(..)
            | Self::CopySpace { .. }
            | Self::ExportMovingSpace { .. }
            | Self::EndMovedSpace { .. }
            | Self::AbandonMove { .. }
            | Self::DeliverMove { .. }
            | Self::DeliverCopy { .. }
            | Self::ReadHeldPackage { .. }
            | Self::ReleaseHeldMove { .. }
            | Self::SettleMove { .. } => Some(TargetCheck::Space),
            Self::StopSpace { .. } => Some(TargetCheck::RunningSpace),
            Self::StopProcesses { .. } => Some(TargetCheck::SettledSpace),
            Self::StopTargets
            | Self::Cleanup { .. }
            | Self::RemovedSpaces
            | Self::RemovedSpace { .. }
            | Self::LocateRemovedSpace { .. }
            | Self::RestoreSpace { .. }
            | Self::DropRestoredSessions { .. }
            | Self::ExportRemovedSpace { .. }
            | Self::TakenRecords { .. }
            | Self::DeliverRemovedSpace { .. }
            | Self::SettleRemovedMove { .. }
            | Self::ImportPackage(..)
            | Self::ReceiveMove { .. } => None,
        }
    }
}

impl Scoped for RemovalMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        const REFUSAL: &str = "scoped processes remove only the spaces below their own; finish their own with shoal done";
        if moves_to_joined_machine(manager, caller, self).await? {
            return Ok(administered(self));
        }
        match self {
            // The check asks for the user's holds too, so a batch refuses
            // before removing any. It removes nothing, so it takes the
            // caller's own space too, as a move of that space checks it.
            RemovalMethod::CheckRemoval { space, .. } => {
                manager
                    .coordination
                    .authorize_held_removal(manager, caller, space, false)
                    .await?;
                Ok(Some(space))
            }
            // The removal takes the user's holds where it releases them.
            RemovalMethod::RemoveSpace { space, .. }
            | RemovalMethod::StartRemoval { space, .. } => manager
                .coordination
                .workspace_below(manager, caller, space, REFUSAL)
                .await
                .map(Some),
            // Into a space its scope reaches, the user's holds taken where
            // it may release them.
            RemovalMethod::AbsorbSpace(absorb) => {
                manager
                    .coordination
                    .ensure_selected_reached(manager, caller, absorb.into.as_mut())
                    .await?;
                const ABSORB: &str = "scoped processes absorb only the spaces below their own";
                (manager
                    .coordination
                    .workspace_below(manager, caller, &mut absorb.space, ABSORB))
                .await
                .map(Some)
            }
            // Its answer confines the spaces to those below the caller's.
            RemovalMethod::StopTargets => Ok(None),
            RemovalMethod::StopSpace { space, .. } => manager
                .coordination
                .workspace_below(
                    manager,
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
            // The machine that keeps the record checks that the caller may
            // restore it, as for a record here.
            RemovalMethod::LocateRemovedSpace { .. } => Ok(None),
            // Below its recorded parent, within the caller's scope; a
            // removed space's record moves to a joined machine with no
            // permission key, as a live space does.
            RemovalMethod::RestoreSpace { space }
            | RemovalMethod::RemovedSpace { space }
            | RemovalMethod::TakenRecords { space, .. }
            | RemovalMethod::DeliverRemovedSpace { space, .. }
            | RemovalMethod::SettleRemovedMove { space, .. } => {
                manager
                    .spaces
                    .ensure_restorable_by(manager, space, caller)
                    .await?;
                Ok(None)
            }
            // A removed space's record, which names no space here to
            // confine it to, goes to a caller that may restore it, as the
            // held move that packed it checked.
            RemovalMethod::ReadHeldPackage { space, .. }
                if manager.spaces.space(space).await.is_err() =>
            {
                manager
                    .spaces
                    .ensure_restorable_by(manager, space, caller)
                    .await?;
                (manager.coordination)
                    .administer(manager, caller, &*self, None)
                    .await?;
                Ok(None)
            }
            // Cleaning up and moving spaces is the `admin` permission's.
            RemovalMethod::Cleanup { .. }
            | RemovalMethod::DropRestoredSessions { .. }
            | RemovalMethod::ExportRemovedSpace { .. }
            | RemovalMethod::CopySpace { .. }
            | RemovalMethod::ExportMovingSpace { .. }
            | RemovalMethod::EndMovedSpace { .. }
            | RemovalMethod::AbandonMove { .. }
            | RemovalMethod::DeliverMove { .. }
            | RemovalMethod::DeliverCopy { .. }
            | RemovalMethod::SettleMove { .. }
            | RemovalMethod::ReadHeldPackage { .. }
            | RemovalMethod::ReleaseHeldMove { .. }
            | RemovalMethod::ImportPackage(_)
            | RemovalMethod::ReceiveMove { .. } => administer(manager, caller, self).await,
        }
    }
}

/// Whether `method` packs, copies, delivers, settles or ends a move to a
/// joined machine, which takes no permission key, only the reach of
/// `caller`'s scope, which the space it acts on must have; ending a space
/// takes the user's holds of it as the caller's `user_holds` permission
/// allows.
async fn moves_to_joined_machine(
    manager: &Manager,
    caller: &Caller,
    method: &mut RemovalMethod,
) -> Result<bool> {
    match method {
        RemovalMethod::ExportMovingSpace { destination, .. } => {
            Ok(matches!(destination, MoveDestination::Machine(_)))
        }
        // A copy for a joined machine goes among the packages delivered
        // from here, as a move there does.
        RemovalMethod::CopySpace { space, file, .. } => {
            manager
                .spaces
                .copies_for_joined_machine(caller, space, file)
                .await
        }
        RemovalMethod::DeliverCopy { space, .. } => {
            manager.spaces.copies_other_than_own(caller, space).await
        }
        RemovalMethod::EndMovedSpace { space } => {
            let joined = manager.spaces.moves_to_joined_machine(space).await?;
            if joined {
                manager
                    .coordination
                    .authorize_held_removal(manager, caller, space, true)
                    .await?;
            }
            Ok(joined)
        }
        RemovalMethod::AbandonMove { space }
        | RemovalMethod::DeliverMove { space }
        | RemovalMethod::SettleMove { space } => {
            manager.spaces.moves_to_joined_machine(space).await
        }
        _ => Ok(false),
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
            *repository = manager
                .coordination
                .own_repository(manager, caller, repository, REFUSAL)
                .await?;
            Some(manager.repositories.repository(&*repository).await?.id)
        }
        None => None,
    };
    manager
        .coordination
        .ensure_selected_reached(manager, caller, administered(method))
        .await?;
    let repository = match (administered(method), repository) {
        (Some(space), None) => {
            manager
                .coordination
                .space_repository(manager, space)
                .await?
        }
        (_, repository) => repository,
    };
    manager
        .coordination
        .administer(manager, caller, &*method, repository)
        .await?;
    Ok(administered(method))
}

/// The existing space `method`, which the `admin` permission governs, acts
/// on: one it moves or copies, or a restored workspace whose kept sessions
/// it drops.
fn administered(method: &mut RemovalMethod) -> Option<&mut SpaceSelector> {
    match method {
        RemovalMethod::ExportMovingSpace { space, .. }
        | RemovalMethod::CopySpace { space, .. }
        | RemovalMethod::EndMovedSpace { space }
        | RemovalMethod::AbandonMove { space }
        | RemovalMethod::DeliverMove { space }
        | RemovalMethod::DeliverCopy { space, .. }
        | RemovalMethod::SettleMove { space }
        | RemovalMethod::ReadHeldPackage { space, .. }
        | RemovalMethod::ReleaseHeldMove { space }
        | RemovalMethod::DropRestoredSessions { space } => Some(space),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
