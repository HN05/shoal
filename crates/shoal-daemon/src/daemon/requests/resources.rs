//! The daemon's answers to [`ResourceMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, ResourceMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ResourceMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ResourceMethod::ResourceAcquire { workspace, request } => manager
            .leases
            .acquire_resource(&**manager, &workspace, request, caller)
            .await?
            .into_body(Body::ResourceLease),
        ResourceMethod::ResourceRelease {
            workspace,
            pool,
            name,
        } => {
            manager
                .leases
                .release_resource(&**manager, &workspace, pool, name)
                .await?;
            Body::Ok
        }
        ResourceMethod::ResourceOverview { workspace } => Body::ResourceOverview(
            manager
                .leases
                .resource_overview(&**manager, &workspace)
                .await?,
        ),
        ResourceMethod::ResourceCatalog { workspace } => Body::ResourceCatalog(
            manager
                .leases
                .resource_catalog(&**manager, &workspace)
                .await?,
        ),
    }))
}

impl Checked for ResourceMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::ResourceAcquire { .. } | Self::ResourceRelease { .. } => {
                Some(TargetCheck::ReadyWorkspace)
            }
            Self::ResourceOverview { .. } | Self::ResourceCatalog { .. } => {
                Some(TargetCheck::Workspace)
            }
        }
    }
}

impl Scoped for ResourceMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ResourceMethod::ResourceAcquire { workspace, .. }
            | ResourceMethod::ResourceRelease { workspace, .. }
            | ResourceMethod::ResourceOverview { workspace }
            | ResourceMethod::ResourceCatalog { workspace } => Some(workspace),
        })
    }
}
