//! The daemon's answers to [`FileMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, FileMethod};

pub(super) async fn handle(manager: &Arc<Manager>, method: FileMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        FileMethod::ListFiles { workspace } => {
            Body::AttachedFiles(manager.workspace_files(&workspace).await?)
        }
        FileMethod::AddFiles { workspace, files } => {
            Body::AttachedFiles(manager.add_workspace_files(&workspace, files).await?)
        }
        FileMethod::RemoveFiles {
            workspace: Some(workspace),
            name,
        } => Body::AttachedFiles(manager.remove_workspace_files(&workspace, name).await?),
        FileMethod::RemoveFiles {
            workspace: None, ..
        } => Body::AttachedFiles(manager.remove_idle_files().await?),
    }))
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
