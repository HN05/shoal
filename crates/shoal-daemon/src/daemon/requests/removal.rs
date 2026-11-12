//! The daemon's answers to [`RemovalMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, ensure};

use super::super::{
    answer::Answer,
    cleanup,
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    client::processes::Selection,
    model::{PresentSpace, StoppedWorkspace},
    process::identity,
    protocol::{Body, QueuedLaunches, RemovalMethod, UntrackedProcesses},
    removal::InspectionPolicy,
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: RemovalMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        RemovalMethod::RemovedWorkspaces => {
            let listed = manager.removed_workspaces().await;
            return Ok(Answer::pages(listed, Body::RemovedWorkspaces));
        }
        RemovalMethod::StopTargets => Body::Spaces(stop_targets(manager).await?),
        RemovalMethod::StopWorkspace {
            workspace,
            acknowledge_unproven,
            launches,
            processes,
        } => {
            let stop = WorkspaceStop {
                acknowledge_unproven,
                launches,
                processes,
            };
            Body::StoppedWorkspace(stop_workspace(manager, &workspace, stop, caller).await?)
        }
        RemovalMethod::StopChat { chat } => {
            let chat = manager.chat(&chat).await?;
            let stopped_by = caller.map(|caller| &caller.space_id);
            Body::StoppedChat(manager.stop_chat_for_resume(&chat, stopped_by).await?)
        }
        RemovalMethod::CheckStop { space } => {
            match manager.space(&space).await? {
                PresentSpace::Workspace(workspace) => {
                    manager.check_lifecycle(&workspace.id).await?
                }
                PresentSpace::Chat(chat) => manager.check_chat_stop(&chat)?,
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
            Body::RestoredWorkspace(Box::new(manager.restore_workspace(&workspace).await?))
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

/// The spaces `shoal stop --all` stops: the workspaces
/// [`Manager::stop_targets`] selects, then the chats
/// [`Manager::chat_stop_targets`] selects.
async fn stop_targets(manager: &Manager) -> Result<Vec<PresentSpace>> {
    let workspaces = manager.stop_targets(QueuedLaunches::Cancel).await?;
    let chats = manager.chat_stop_targets().await?.into_iter();
    Ok((workspaces.into_iter().map(PresentSpace::Workspace))
        .chain(chats.map(PresentSpace::Chat))
        .collect())
}

/// What a [`RemovalMethod::StopWorkspace`] request asks of the stop.
struct WorkspaceStop {
    acknowledge_unproven: bool,
    launches: QueuedLaunches,
    processes: UntrackedProcesses,
}

/// Stop `workspace`'s agents and commands as `caller`, then its untracked
/// processes as `stop` says, reporting whether the stop saved work for
/// `shoal resume`; a CI job's workspace has its run cancelled instead.
async fn stop_workspace(
    manager: &Manager,
    workspace: &SpaceSelector,
    stop: WorkspaceStop,
    caller: Option<&Caller>,
) -> Result<StoppedWorkspace> {
    let stopped_by = caller.map(|caller| &caller.space_id);
    let mut stopped = manager
        .stop_workspace_manually(
            workspace,
            stop.acknowledge_unproven,
            stopped_by,
            stop.launches,
        )
        .await?;
    if stopped.cancelled_run.is_some() {
        return Ok(stopped);
    }
    if let UntrackedProcesses::Stop { requester } = stop.processes {
        let untracked = manager
            .stop_space_processes(workspace, Selection::Processes, identity_of(requester))
            .await?;
        stopped.other_processes = untracked.processes;
    }
    // A read that fails reports nothing saved rather than failing the stop.
    stopped.saved = (manager.saved_work(workspace).await).is_ok_and(|saved| !saved.is_empty());
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
            // The check refuses holds too, so a batch refuses before removing any.
            RemovalMethod::CheckRemoval { workspace, .. }
            | RemovalMethod::RemoveWorkspace { workspace, .. }
            | RemovalMethod::StartRemoval { workspace, .. } => manager
                .unheld_workspace_below(caller, workspace, REFUSAL)
                .await
                .map(Some),
            RemovalMethod::StopWorkspace {
                workspace,
                acknowledge_unproven,
                ..
            } => {
                ensure!(
                    !*acknowledge_unproven,
                    "only the user can confirm that an execution Shoal cannot prove ended did; ask the user to stop this workspace"
                );
                manager
                    .workspace_below(
                        caller,
                        workspace,
                        "workspace processes stop only the workspaces below theirs",
                    )
                    .await
                    .map(Some)
            }
            RemovalMethod::StopChat { chat } => manager
                .workspace_below(
                    caller,
                    chat,
                    "scoped processes stop only the spaces below theirs",
                )
                .await
                .map(Some),
            // Its own processes too: a scoped caller's shells and their
            // commands descend from its execution, which keeps them.
            RemovalMethod::StopProcesses { space, .. } => Ok(Some(space)),
            RemovalMethod::CheckStop { space } => Ok(Some(space)),
            _ => Err(denied()),
        }
    }
}
