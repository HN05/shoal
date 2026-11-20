//! The daemon's answers to [`DemoMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, DemoMethod};
use shoal_protocol::{scope_tokens::ScopeToken, space_names::SpaceSelector};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: DemoMethod,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        DemoMethod::ListDemos { workspace } => {
            Body::Demos(manager.demos(workspace.as_ref()).await?)
        }
        DemoMethod::DemoProgress { workspace, launch } => {
            Body::DemoProgress(manager.demo_progress(&workspace, &launch).await?)
        }
        DemoMethod::StopDemos { workspace, name } => {
            manager.stop_demos(&workspace, name).await?;
            Body::Ok
        }
        DemoMethod::OpenDemoStream { workspace, name } => {
            let open = manager.open_demo(&workspace, &name).await?;
            // Another machine's stream is checked there as it goes, its
            // token here standing for its caller only while it opens.
            let scope = scope.filter(|token| manager.hub.vouched_caller(token).is_none());
            let scope = scope.cloned();
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                manager.relay_demo(stream, request_id, (open, scope), guard)
            }));
        }
    }))
}

impl Scoped for DemoMethod {
    async fn scope(&mut self, _: &Manager, caller: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            DemoMethod::ListDemos { workspace } => Some(caller.own_by_default(workspace)),
            DemoMethod::DemoProgress { workspace, .. }
            | DemoMethod::StopDemos { workspace, .. }
            | DemoMethod::OpenDemoStream { workspace, .. } => Some(workspace),
        })
    }
}
