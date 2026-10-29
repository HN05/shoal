//! The daemon's answers to [`FileMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

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
    }))
}

impl Scoped for FileMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            FileMethod::ListFiles { workspace } | FileMethod::AddFiles { workspace, .. } => {
                Some(workspace)
            }
        })
    }
}
