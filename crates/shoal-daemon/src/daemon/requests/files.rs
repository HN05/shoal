//! The daemon's answers to [`FileMethod`] requests, who may send them, and what
//! each requires of the space it acts on.
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, FileMethod};
use shoal_protocol::space_names::SpaceSelector;

/// Every answer but a file's bytes lists files, which no limit keeps to one
/// frame, so each is sent in pages; the bytes come in frames as the copy
/// is read.
pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: FileMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    let files = match method {
        FileMethod::ListFiles { space } => manager.attached_files(&space).await,
        FileMethod::FetchedDirectory { space } => {
            let directory = manager.fetched_directory(caller, &space).await?;
            return Ok(Answer::Body(Body::FetchedDirectory(directory)));
        }
        FileMethod::ReadFile { space, id } => {
            let copy = manager.open_attached_file(&space, id).await?;
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                manager.stream_attached_file(stream, request_id, copy, guard)
            }));
        }
        FileMethod::AddFiles { space, files } => {
            let attacher = manager.attacher(caller).await;
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

impl Checked for FileMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::ListFiles { .. } | Self::ReadFile { .. } | Self::AddFiles { .. } => {
                Some(TargetCheck::Space)
            }
            Self::RemoveFiles { .. } | Self::FetchedDirectory { .. } => None,
        }
    }
}

impl Scoped for FileMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        match self {
            FileMethod::AddFiles { space, .. } => Ok(Some(space)),
            // It finds only the spaces the caller sees.
            FileMethod::FetchedDirectory { .. } => Ok(None),
            // A post's files reach whoever reads its thread, wherever they
            // are kept.
            FileMethod::ReadFile { id, .. }
                if manager
                    .coordination
                    .reads_posted_file(manager, caller, *id)
                    .await? =>
            {
                Ok(None)
            }
            FileMethod::ListFiles { space }
            | FileMethod::ReadFile { space, .. }
            | FileMethod::RemoveFiles {
                space: Some(space), ..
            } => {
                if manager.space(space).await.is_ok() {
                    return Ok(Some(space));
                }
                narrow_to_removed(manager, caller, space).await?;
                Ok(None)
            }
            FileMethod::RemoveFiles { space: None, .. } => bail!(
                "workspace processes can delete only the files of their own space and those below it"
            ),
        }
    }
}

/// Point `workspace` at the removed space it names, whose results a
/// caller may reach when the space was below the caller's own or one below
/// it.
async fn narrow_to_removed(
    manager: &Manager,
    caller: &Caller,
    workspace: &mut SpaceSelector,
) -> Result<()> {
    let space = manager.file_space(workspace).await?;
    let below = match &space.parent_id {
        Some(parent) => {
            manager
                .coordination
                .reaches(manager, caller, parent)
                .await?
        }
        None => manager
            .coordination
            .reach_root(Some(caller))
            .await?
            .is_none(),
    };
    ensure!(below, "space {} is outside this space's scope", space.id);
    *workspace = SpaceSelector::id(&space.id);
    Ok(())
}

#[cfg(test)]
mod tests;
