//! The daemon's answers to [`RemovalMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, ensure};

use super::super::{
    answer::Answer,
    cleanup,
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::{
    process::identity,
    protocol::{Body, QueuedLaunches, RemovalMethod},
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
        RemovalMethod::StopTargets => {
            Body::Workspaces(manager.stop_targets(QueuedLaunches::Cancel).await?)
        }
        RemovalMethod::StopWorkspace {
            workspace,
            acknowledge_unproven,
            launches,
        } => {
            let stopped_by = caller.map(|caller| caller.space_id.as_str());
            Body::QueueEntries(
                manager
                    .stop_workspace_manually(&workspace, acknowledge_unproven, stopped_by, launches)
                    .await?,
            )
        }
        RemovalMethod::CheckStop { workspace } => {
            let id = manager.workspace(&workspace).await?.id;
            manager.check_lifecycle(&id).await?;
            Body::Ok
        }
        RemovalMethod::StopProcesses {
            space,
            selection,
            requester,
        } => {
            let requester = requester.and_then(|pid| identity::capture(pid).ok().flatten());
            Body::StoppedProcesses(
                manager
                    .stop_space_processes(&space, selection, requester)
                    .await?,
            )
        }
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
        RemovalMethod::Cleanup { dry_run, selection } => {
            Body::Cleanup(cleanup::on_request(manager, dry_run, &selection).await?)
        }
        RemovalMethod::RestoreWorkspace { workspace } => {
            Body::RestoredWorkspace(Box::new(manager.restore_workspace(&workspace).await?))
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
        } => Body::RestoredWorkspace(Box::new(
            manager
                .import_workspace(
                    &repository,
                    bundle.as_deref(),
                    prerequisite.as_deref(),
                    *workspace,
                )
                .await?,
        )),
    }))
}

impl Scoped for RemovalMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        const REFUSAL: &str = "workspace processes remove only the workspaces split from theirs; finish their own with shoal done";
        match self {
            // The check refuses holds too, so a batch refuses before removing any.
            RemovalMethod::CheckRemoval { workspace, .. }
            | RemovalMethod::RemoveWorkspace { workspace, .. } => manager
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
            // Its own processes too: a scoped caller's shells and their
            // commands descend from its execution, which keeps them.
            RemovalMethod::StopProcesses { space, .. } => Ok(Some(space)),
            RemovalMethod::CheckStop { workspace } => Ok(Some(workspace)),
            _ => Err(denied()),
        }
    }
}
