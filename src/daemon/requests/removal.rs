//! The daemon's answers to [`RemovalMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    cleanup,
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::{
    protocol::{Body, RemovalMethod},
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
        RemovalMethod::StopTargets => Body::Workspaces(manager.stop_targets().await?),
        RemovalMethod::StopWorkspace {
            workspace,
            acknowledge_unproven,
        } => {
            manager
                .stop_workspace_manually(&workspace, acknowledge_unproven)
                .await?;
            Body::Ok
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
            Body::RestoredWorkspace(manager.restore_workspace(&workspace).await?)
        }
        RemovalMethod::ExportWorkspace {
            workspace,
            unsaved,
            file,
        } => Body::ExportedWorkspace(
            manager
                .export_workspace(&workspace, unsaved, InspectionPolicy::GitOnly, file)
                .await?,
        ),
        RemovalMethod::ImportWorkspace {
            repository,
            bundle,
            prerequisite,
            workspace,
        } => Body::RestoredWorkspace(
            manager
                .import_workspace(
                    &repository,
                    bundle.as_deref(),
                    prerequisite.as_deref(),
                    *workspace,
                )
                .await?,
        ),
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
            _ => Err(denied()),
        }
    }
}
