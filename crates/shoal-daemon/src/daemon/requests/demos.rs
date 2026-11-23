//! The daemon's answers to [`DemoMethod`] requests, who may send them, and what
//! each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, DemoMethod};
use shoal_protocol::{demos::Running, scope_tokens::ScopeToken, space_names::SpaceSelector};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: DemoMethod,
    caller: Option<&Caller>,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        DemoMethod::ListDemos { workspace } => {
            Body::Demos(listed(manager, workspace.as_ref(), scope).await?)
        }
        DemoMethod::DemoProgress { workspace, launch } => Body::DemoProgress(
            manager
                .demos
                .progress(&**manager, &workspace, &launch)
                .await?,
        ),
        DemoMethod::StopDemos { workspace, name } => {
            manager.demos.stop(&**manager, &workspace, name).await?;
            Body::Ok
        }
        DemoMethod::OpenDemoStream {
            workspace,
            name,
            terminal,
        } => {
            let open = manager
                .open_demo(&workspace, &name, &terminal, caller)
                .await?;
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

impl Checked for DemoMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::DemoProgress { .. } | Self::StopDemos { .. } | Self::OpenDemoStream { .. } => {
                Some(TargetCheck::Space)
            }
            Self::ListDemos { .. } => None,
        }
    }
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

/// The running demos of `workspace`, or of every workspace, each naming
/// this machine when another machine asked for them.
async fn listed(
    manager: &Manager,
    workspace: Option<&SpaceSelector>,
    scope: Option<&ScopeToken>,
) -> Result<Vec<Running>> {
    let mut demos = manager.demos.list(manager, workspace).await?;
    let machine = super::forwarded::answering_machine(manager, scope).await?;
    for demo in &mut demos {
        demo.machine.clone_from(&machine);
    }
    Ok(demos)
}
