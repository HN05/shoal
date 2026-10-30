//! The daemon's answers to [`FileMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, FileMethod};

/// Every answer lists files, which no limit keeps to one frame, so each is
/// sent in pages.
pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: FileMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    let files = match method {
        FileMethod::ListFiles { workspace } => manager.workspace_files(&workspace).await,
        FileMethod::AddFiles { workspace, files } => {
            let attacher = manager.attacher(caller).await;
            manager
                .add_workspace_files(&workspace, files, &attacher)
                .await
        }
        FileMethod::RemoveFiles {
            workspace: Some(workspace),
            names,
            ids,
        } => manager.remove_workspace_files(&workspace, names, ids).await,
        FileMethod::RemoveFiles {
            workspace: None,
            names,
            ids,
        } => {
            // Names and records belong to one workspace's files.
            ensure!(
                names.is_empty() && ids.is_empty(),
                "name the workspace whose files to delete by name"
            );
            manager.remove_idle_files().await
        }
    };
    Ok(Answer::pages(files, Body::AttachedFiles))
}

impl Scoped for FileMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        match self {
            FileMethod::AddFiles { workspace, .. } => Ok(Some(workspace)),
            FileMethod::ListFiles { workspace }
            | FileMethod::RemoveFiles {
                workspace: Some(workspace),
                ..
            } => {
                if manager.workspace(workspace).await.is_ok() {
                    return Ok(Some(workspace));
                }
                narrow_to_removed(manager, caller, workspace).await?;
                Ok(None)
            }
            FileMethod::RemoveFiles {
                workspace: None, ..
            } => bail!(
                "workspace processes can delete only the files of their own workspace and those split from it"
            ),
        }
    }
}

/// Point `workspace` at the removed workspace it names, whose results a
/// caller may reach when the workspace was split from the caller's own or
/// one below it.
async fn narrow_to_removed(
    manager: &Manager,
    caller: &Caller,
    workspace: &mut String,
) -> Result<()> {
    let owner = manager.file_owner(workspace).await?;
    let below = match &owner.parent_id {
        Some(parent) => manager.within_subtree(&caller.workspace_id, parent).await?,
        None => false,
    };
    ensure!(
        below,
        "workspace processes can only access their own workspace and those split from it"
    );
    *workspace = owner.id;
    Ok(())
}

#[cfg(test)]
mod tests;
