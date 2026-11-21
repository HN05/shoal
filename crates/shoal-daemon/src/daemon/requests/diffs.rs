//! The daemon's answers to [`DiffMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
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
            let mut base = manager.diff_base(&workspace).await?;
            base.machine = super::forwarded::answering_machine(manager, scope).await?;
            Body::DiffBase(base)
        }
        DiffMethod::Conflicts { workspace, target } => {
            Body::Conflicts(manager.check_conflicts(&workspace, target).await?)
        }
        DiffMethod::Diff { workspace, display } => {
            let diff = manager.start_diff(&workspace, display).await?;
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                manager.stream_diff(stream, request_id, diff, guard)
            }));
        }
    }))
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
