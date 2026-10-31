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
        FileMethod::ListFiles { space } => manager.attached_files(&space).await,
        FileMethod::AddFiles {
            space,
            files,
            sender_chat,
        } => {
            let attacher = manager.attacher(caller, sender_chat.as_deref()).await;
            manager.add_attached_files(&space, files, &attacher).await
        }
        FileMethod::RemoveFiles {
            space: Some(space),
            names,
            ids,
        } => manager.remove_attached_files(&space, names, ids).await,
        FileMethod::RemoveFiles {
            space: None,
            names,
            ids,
        } => {
            // Names and records belong to one space's files.
            ensure!(
                names.is_empty() && ids.is_empty(),
                "name the workspace or chat whose files to delete by name"
            );
            manager.remove_idle_files().await
        }
    };
    Ok(Answer::pages(files, Body::AttachedFiles))
}

impl Scoped for FileMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        match self {
            FileMethod::AddFiles { space, .. } => Ok(Some(space)),
            FileMethod::ListFiles { space }
            | FileMethod::RemoveFiles {
                space: Some(space), ..
            } => {
                if manager.workspace(space).await.is_ok() {
                    return Ok(Some(space));
                }
                narrow_to_removed(manager, caller, space).await?;
                Ok(None)
            }
            FileMethod::RemoveFiles { space: None, .. } => bail!(
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
    let space = manager.file_space(workspace).await?;
    let below = match &space.parent_id {
        Some(parent) => manager.within_subtree(&caller.workspace_id, parent).await?,
        None => false,
    };
    ensure!(
        below,
        "workspace processes can only access their own workspace and those split from it"
    );
    *workspace = space.id;
    Ok(())
}

#[cfg(test)]
mod tests;
