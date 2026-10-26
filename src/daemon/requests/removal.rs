//! The daemon's answers to [`RemovalMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    cleanup,
    scope::{Caller, Scoped, denied},
    workspace::{Manager, StopRequest},
};
use crate::{
    protocol::{Body, RemovalMethod},
    removal::InspectionPolicy,
};

pub(super) async fn handle(manager: &Arc<Manager>, method: RemovalMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        RemovalMethod::RemovedWorkspaces => {
            let listed = manager.removed_workspaces().await;
            return Ok(Answer::pages(listed, Body::RemovedWorkspaces));
        }
        RemovalMethod::StopWorkspace {
            workspace,
            acknowledge_unproven,
        } => {
            if acknowledge_unproven {
                manager
                    .stop_workspace_acknowledged(&workspace, StopRequest::manual())
                    .await?;
            } else {
                manager
                    .stop_workspace(&workspace, StopRequest::manual())
                    .await?;
            }
            Body::Ok
        }
        RemovalMethod::CheckRemoval {
            workspace,
            include_changes,
        } => {
            let mut check = manager
                .check_removal(&workspace, InspectionPolicy::GitOnly)
                .await?;
            if include_changes && check.dirty {
                check.load_changed_files().await?;
                check.unsaved_files_archived =
                    manager.archives_unsaved_files(&check.workspace).await?;
            }
            Body::RemovalCheck(check)
        }
        RemovalMethod::RemoveWorkspace { workspace, unsaved } => Body::RemovalResult(
            manager
                .remove_workspace(&workspace, unsaved, InspectionPolicy::GitOnly)
                .await?,
        ),
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
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Err(denied())
    }
}
