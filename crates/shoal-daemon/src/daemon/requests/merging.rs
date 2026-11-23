//! The daemon's answers to [`MergingMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, MergingMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: MergingMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        MergingMethod::MergePath { space } => {
            Body::MergePath(manager.links.merge_path(&**manager, &space).await?)
        }
        MergingMethod::PreviewMerge {
            workspace,
            push,
            method,
            message,
        } => {
            let options = crate::model::MergeOptions {
                push,
                method,
                message,
                confirmed: None,
            };
            Body::MergePreview(
                manager
                    .spaces
                    .preview_merge(&**manager, &workspace, &options, caller)
                    .await?,
            )
        }
    }))
}

impl Checked for MergingMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::MergePath { .. } => Some(TargetCheck::Space),
            Self::PreviewMerge { .. } => Some(TargetCheck::Workspace),
        }
    }
}

impl Scoped for MergingMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            MergingMethod::MergePath { space } => Some(space),
            MergingMethod::PreviewMerge { workspace, .. } => Some(workspace),
        })
    }
}
