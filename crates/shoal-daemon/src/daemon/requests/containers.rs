//! The daemon's answers to [`ContainerMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, ContainerMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ContainerMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ContainerMethod::ContainerCatalog { workspace } => {
            Body::ContainerCatalog(manager.container_catalog(&workspace).await?)
        }
        ContainerMethod::ContainerOverview { workspace } => {
            Body::ContainerOverview(manager.container_overview(workspace.as_ref()).await?)
        }
        ContainerMethod::AcquireContainers { workspace, reason } => manager
            .acquire_containers(&workspace, reason, caller)
            .await?
            .into_body(),
        ContainerMethod::ReleaseContainers { workspace } => {
            manager.release_containers(&workspace).await?;
            Body::Ok
        }
        ContainerMethod::SetUpContainers {
            workspace,
            for_agent,
        } => {
            (manager.set_up_containers(&workspace, for_agent, caller)).await?;
            Body::Ok
        }
        ContainerMethod::StartAgentContainers { workspace } => {
            manager.start_agent_containers(&workspace, caller).await?;
            Body::Ok
        }
    }))
}

impl Scoped for ContainerMethod {
    async fn scope(&mut self, _: &Manager, caller: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ContainerMethod::ContainerCatalog { workspace }
            | ContainerMethod::AcquireContainers { workspace, .. }
            | ContainerMethod::ReleaseContainers { workspace }
            | ContainerMethod::SetUpContainers { workspace, .. }
            | ContainerMethod::StartAgentContainers { workspace } => Some(workspace),
            ContainerMethod::ContainerOverview { workspace } => {
                Some(caller.own_by_default(workspace))
            }
        })
    }
}
