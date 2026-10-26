//! The daemon's answers to [`ResourceMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, ResourceMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ResourceMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ResourceMethod::ResourceAcquire { workspace, request } => manager
            .acquire_resource(&workspace, request, caller)
            .await?
            .into_body(Body::ResourceLease),
        ResourceMethod::ResourceRelease {
            workspace,
            pool,
            name,
        } => {
            manager.release_resource(&workspace, pool, name).await?;
            Body::Ok
        }
        ResourceMethod::ResourceOverview { workspace } => {
            Body::ResourceOverview(manager.resource_overview(&workspace).await?)
        }
        ResourceMethod::ResourceCatalog { workspace } => {
            Body::ResourceCatalog(manager.resource_catalog(&workspace).await?)
        }
    }))
}

impl Scoped for ResourceMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            ResourceMethod::ResourceAcquire { workspace, .. }
            | ResourceMethod::ResourceRelease { workspace, .. }
            | ResourceMethod::ResourceOverview { workspace }
            | ResourceMethod::ResourceCatalog { workspace } => Some(workspace),
        })
    }
}
