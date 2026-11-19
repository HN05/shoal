//! The daemon's answers to [`PortMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, PortMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: PortMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        PortMethod::PortAcquire {
            workspace,
            name,
            request,
        } => manager
            .leases
            .acquire_port(&**manager, &workspace, name, request, caller)
            .await?
            .into_body(),
        PortMethod::PortRelease { workspace, name } => {
            manager
                .leases
                .release_port(&**manager, &workspace, name)
                .await?;
            Body::Ok
        }
        PortMethod::PortOverview { workspace } => {
            Body::PortOverview(manager.leases.port_overview(&**manager, &workspace).await?)
        }
        PortMethod::PortCatalog { workspace } => {
            Body::PortCatalog(manager.leases.port_catalog(&**manager, &workspace).await?)
        }
    }))
}

impl Scoped for PortMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            PortMethod::PortAcquire { workspace, .. }
            | PortMethod::PortRelease { workspace, .. }
            | PortMethod::PortOverview { workspace }
            | PortMethod::PortCatalog { workspace } => Some(workspace),
        })
    }
}
