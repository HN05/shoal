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
        FileMethod::ListFiles { owner } => manager.attached_files(&owner).await,
        FileMethod::AddFiles {
            owner,
            files,
            from_chat,
        } => {
            let attacher = manager.attacher(caller, from_chat.as_deref()).await;
            manager.add_attached_files(&owner, files, &attacher).await
        }
        FileMethod::RemoveFiles {
            owner: Some(owner),
            names,
            ids,
        } => manager.remove_attached_files(&owner, names, ids).await,
        FileMethod::RemoveFiles {
            owner: None,
            names,
            ids,
        } => {
            // Names and records belong to one owner's files.
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
            FileMethod::AddFiles { owner, .. } => Ok(Some(owner)),
            FileMethod::ListFiles { owner }
            | FileMethod::RemoveFiles {
                owner: Some(owner), ..
            } => {
                if manager.workspace(owner).await.is_ok() {
                    return Ok(Some(owner));
                }
                narrow_to_removed(manager, caller, owner).await?;
                Ok(None)
            }
            FileMethod::RemoveFiles { owner: None, .. } => bail!(
                "workspace processes can delete only the files of their own workspace and those split from it"
            ),
        }
    }
}

/// Point `workspace` at the removed workspace it names, whose results a
/// caller may reach when the workspace was split from the caller's own or
/// one below it; a chat has no parent, so its files stay out of reach.
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
