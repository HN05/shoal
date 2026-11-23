//! The daemon's answers to [`DiffMethod`] requests, who may send them, and what
//! each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, DiffMethod};
use shoal_protocol::{scope_tokens::ScopeToken, space_names::SpaceSelector};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: DiffMethod,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        DiffMethod::DiffBase { workspace } => {
            let mut base = manager.spaces.diff_base(&**manager, &workspace).await?;
            base.machine = super::forwarded::answering_machine(manager, scope).await?;
            Body::DiffBase(base)
        }
        DiffMethod::Conflicts { workspace, target } => Body::Conflicts(
            manager
                .spaces
                .check_conflicts(&**manager, &workspace, target)
                .await?,
        ),
        DiffMethod::Diff { workspace, display } => {
            let diff = manager
                .spaces
                .start_diff(&**manager, &workspace, display)
                .await?;
            let manager = manager.clone();
            return Ok(Answer::connection(
                move |stream, request_id, guard| async move {
                    manager
                        .spaces
                        .stream_diff(stream, request_id, diff, guard)
                        .await
                },
            ));
        }
    }))
}

impl Checked for DiffMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::DiffBase { .. } | Self::Conflicts { .. } | Self::Diff { .. } => {
                Some(TargetCheck::Workspace)
            }
        }
    }
}

impl Scoped for DiffMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            DiffMethod::DiffBase { workspace }
            | DiffMethod::Conflicts { workspace, .. }
            | DiffMethod::Diff { workspace, .. } => Some(workspace),
        })
    }
}
