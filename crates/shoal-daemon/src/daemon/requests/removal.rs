//! The daemon's answers to [`RemovalMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    cleanup,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    client::processes::Selection,
    model::{PresentSpace, StoppedSpace},
    process::identity,
    protocol::{Body, QueuedLaunches, RemovalMethod, UntrackedProcesses},
    removal::InspectionPolicy,
};

/// Whether `method` moves a space through a lifecycle state that an exit
/// would interrupt, which an exit that waits for work in progress holds back.
fn changes_lifecycle(method: &RemovalMethod) -> bool {
    match method {
        RemovalMethod::StopSpace { .. }
        | RemovalMethod::RemoveWorkspace { .. }
        | RemovalMethod::StartRemoval { .. }
        | RemovalMethod::RestoreWorkspace { .. }
        | RemovalMethod::ExportSpace { .. }
        | RemovalMethod::ExportMovingSpace { .. }
        | RemovalMethod::EndMovedSpace { .. }
        | RemovalMethod::ImportWorkspace { .. } => true,
        RemovalMethod::Cleanup { dry_run, .. } => !dry_run,
        RemovalMethod::StopTargets
        | RemovalMethod::CheckStop { .. }
        | RemovalMethod::StopProcesses { .. }
        | RemovalMethod::CheckRemoval { .. }
        | RemovalMethod::RemovedWorkspaces
        | RemovalMethod::DropRestoredSessions { .. } => false,
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
        RemovalMethod::RemovedWorkspaces => {
            let listed = manager.removed_workspaces().await;
            return Ok(Answer::pages(listed, Body::RemovedWorkspaces));
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
            if let PresentSpace::Workspace(workspace) = manager.space(&space).await? {
                manager.check_lifecycle(&workspace.id).await?
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
            workspace,
            include_changes,
        } => Body::RemovalCheck(
            manager
                .check_manual_removal(&workspace, include_changes)
                .await?,
        ),
        RemovalMethod::RemoveWorkspace { workspace, unsaved } => {
            Body::RemovalResult(match caller {
                Some(_) => {
                    manager
                        .remove_unheld_workspace(&workspace, unsaved, InspectionPolicy::GitOnly)
                        .await?
                }
                None => {
                    manager
                        .remove_workspace(&workspace, unsaved, InspectionPolicy::GitOnly)
                        .await?
                }
            })
        }
        RemovalMethod::StartRemoval { workspace, unsaved } => {
            manager
                .start_workspace_removal(&workspace, unsaved, caller)
                .await?;
            Body::Ok
        }
        RemovalMethod::Cleanup { dry_run, selection } => {
            Body::Cleanup(cleanup::on_request(manager, dry_run, &selection).await?)
        }
        RemovalMethod::RestoreWorkspace { workspace } => {
            Body::RestoredWorkspace(Box::new(match caller {
                Some(caller) => manager.restore_workspace_below(&workspace, caller).await?,
                None => manager.restore_workspace(&workspace).await?,
            }))
        }
        RemovalMethod::DropRestoredSessions { workspace } => {
            manager.drop_restored_sessions(&workspace).await?;
            Body::Ok
        }
        RemovalMethod::ExportSpace {
            space,
            unsaved,
            file,
        } => Body::ExportedSpace(
            manager
                .export_space(&space, unsaved, InspectionPolicy::GitOnly, file)
                .await?,
        ),
        RemovalMethod::ExportMovingSpace { space, file } => {
            Body::ExportedSpace(manager.export_moving_space(&space, file).await?)
        }
        RemovalMethod::EndMovedSpace {
            space,
            unsaved,
            checkpoint,
        } => Body::EndedSpace(manager.end_moved_space(&space, unsaved, checkpoint).await?),
        RemovalMethod::ImportWorkspace {
            repository,
            bundle,
            prerequisite,
            workspace,
            copy,
            session_files,
        } => Body::RestoredWorkspace(Box::new(
            manager
                .import_workspace(
                    &repository,
                    bundle.as_deref(),
                    prerequisite.as_deref(),
                    *workspace,
                    copy,
                    &session_files,
                )
                .await?,
        )),
    }))
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
        const REFUSAL: &str = "workspace processes remove only the workspaces split from theirs; finish their own with shoal done";
        match self {
            // The check asks for the user's holds too, so a batch refuses
            // before removing any.
            RemovalMethod::CheckRemoval { workspace, .. } => manager
                .unheld_workspace_below(caller, workspace, REFUSAL, false)
                .await
                .map(Some),
            RemovalMethod::RemoveWorkspace { workspace, .. }
            | RemovalMethod::StartRemoval { workspace, .. } => manager
                .unheld_workspace_below(caller, workspace, REFUSAL, true)
                .await
                .map(Some),
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
            // Cleaning up, restoring removed workspaces and moving spaces is
            // the `admin` permission's.
            RemovalMethod::Cleanup { .. }
            | RemovalMethod::RemovedWorkspaces
            | RemovalMethod::RestoreWorkspace { .. }
            | RemovalMethod::DropRestoredSessions { .. }
            | RemovalMethod::ExportSpace { .. }
            | RemovalMethod::ExportMovingSpace { .. }
            | RemovalMethod::EndMovedSpace { .. }
            | RemovalMethod::ImportWorkspace { .. } => administer(manager, caller, self).await,
        }
    }
}

/// Let `caller` send `method`, which the `admin` permission governs, in its
/// own repository where it imports into one, returning the space it moves,
/// which its scope must reach before it asks approval.
async fn administer<'a>(
    manager: &Manager,
    caller: &Caller,
    method: &'a mut RemovalMethod,
) -> Result<Option<&'a mut SpaceSelector>> {
    let repository = match method {
        // A restore opens the workspace below the caller, which must take it.
        RemovalMethod::RestoreWorkspace { workspace } => {
            Some(manager.restorable_below(workspace, caller).await?)
        }
        RemovalMethod::ImportWorkspace { repository, .. } => {
            const REFUSAL: &str = "scoped processes import only into their own repository";
            *repository = manager.own_repository(caller, repository, REFUSAL).await?;
            Some(manager.repository(&*repository).await?.id)
        }
        _ => None,
    };
    manager
        .ensure_selected_reached(caller, moved(method))
        .await?;
    let repository = match (moved(method), repository) {
        (Some(space), None) => manager.space_repository(space).await?,
        (_, repository) => repository,
    };
    manager.administer(caller, &*method, repository).await?;
    Ok(moved(method))
}

/// The space `method` moves, if it moves one.
fn moved(method: &mut RemovalMethod) -> Option<&mut SpaceSelector> {
    match method {
        RemovalMethod::ExportSpace { space, .. }
        | RemovalMethod::ExportMovingSpace { space, .. }
        | RemovalMethod::EndMovedSpace { space, .. } => Some(space),
        _ => None,
    }
}
