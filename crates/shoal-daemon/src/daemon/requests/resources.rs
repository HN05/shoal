//! The daemon's answers to [`ResourceMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
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
