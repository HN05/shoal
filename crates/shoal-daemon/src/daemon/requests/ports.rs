//! The daemon's answers to [`PortMethod`] requests, who may send them, and what
//! each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
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

impl Checked for PortMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::PortAcquire { .. } | Self::PortRelease { .. } => {
                Some(TargetCheck::ReadyWorkspace)
            }
            Self::PortOverview { .. } | Self::PortCatalog { .. } => Some(TargetCheck::Workspace),
        }
    }
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
