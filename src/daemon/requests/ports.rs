//! The daemon's answers to [`PortMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, PortMethod};

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
            .acquire_port(&workspace, name, request, caller)
            .await?
            .into_body(),
        PortMethod::PortRelease { workspace, name } => {
            manager.release_port(&workspace, name).await?;
            Body::Ok
        }
        PortMethod::PortOverview { workspace } => {
            Body::PortOverview(manager.port_overview(&workspace).await?)
        }
        PortMethod::PortCatalog { workspace } => {
            Body::PortCatalog(manager.port_catalog(&workspace).await?)
        }
    }))
}

impl Scoped for PortMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            PortMethod::PortAcquire { workspace, .. }
            | PortMethod::PortRelease { workspace, .. }
            | PortMethod::PortOverview { workspace }
            | PortMethod::PortCatalog { workspace } => Some(workspace),
        })
    }
}
