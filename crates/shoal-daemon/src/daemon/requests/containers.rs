//! The daemon's answers to [`ContainerMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, ContainerMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ContainerMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ContainerMethod::ContainerCatalog { workspace } => Body::ContainerCatalog(
            manager
                .leases
                .container_catalog(&**manager, &workspace)
                .await?,
        ),
        ContainerMethod::ContainerOverview { workspace } => Body::ContainerOverview(
            manager
                .leases
                .container_overview(&**manager, workspace.as_ref())
                .await?,
        ),
        ContainerMethod::ContainerAcquire { workspace, reason } => manager
            .leases
            .acquire_containers(&**manager, &workspace, reason, caller)
            .await?
            .into_body(),
        ContainerMethod::ContainerRelease { workspace } => {
            manager
                .leases
                .release_containers(&**manager, &workspace)
                .await?;
            Body::Ok
        }
        ContainerMethod::ContainerSetUp {
            workspace,
            for_agent,
        } => {
            (manager
                .leases
                .set_up_containers(&**manager, &workspace, for_agent, caller))
            .await?;
            Body::Ok
        }
        ContainerMethod::ContainerAgentStart { workspace } => {
            manager
                .leases
                .start_agent_containers(&**manager, &workspace, caller)
                .await?;
            Body::Ok
        }
    }))
}

impl Checked for ContainerMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::ContainerAcquire { .. } | Self::ContainerRelease { .. } => {
                Some(TargetCheck::ReadyWorkspace)
            }
            Self::ContainerCatalog { .. }
            | Self::ContainerSetUp { .. }
            | Self::ContainerAgentStart { .. } => Some(TargetCheck::Workspace),
            Self::ContainerOverview { .. } => None,
        }
    }
}

impl Scoped for ContainerMethod {
    async fn scope(&mut self, _: &Manager, caller: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ContainerMethod::ContainerCatalog { workspace }
            | ContainerMethod::ContainerAcquire { workspace, .. }
            | ContainerMethod::ContainerRelease { workspace }
            | ContainerMethod::ContainerSetUp { workspace, .. }
            | ContainerMethod::ContainerAgentStart { workspace } => Some(workspace),
            ContainerMethod::ContainerOverview { workspace } => {
                Some(caller.own_by_default(workspace))
            }
        })
    }
}
