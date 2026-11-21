//! The daemon's answers to [`PermissionMethod`] requests, and who may send
//! them.
use std::sync::Arc;

use anyhow::{Context as _, Result};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, PermissionMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: PermissionMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        PermissionMethod::LaunchPermissions {
            workspace,
            mode,
            narrowing,
        } => {
            let workspace = manager.workspace(&workspace).await?;
            Body::Permissions(
                manager
                    .launch_permissions(workspace, &mode, narrowing)
                    .await?,
            )
        }
        PermissionMethod::HeldPermissions { repository } => {
            let caller = caller.context("the user holds every permission")?;
            let repository = match repository {
                Some(repository) => Some(manager.repository(repository).await?.id),
                None => None,
            };
            let held = manager.caller_permissions(caller, repository.as_ref());
            Body::Permissions(held.await?)
        }
    }))
}

impl Scoped for PermissionMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            PermissionMethod::LaunchPermissions { workspace, .. } => Some(workspace),
            // The handler answers with the caller's own space's.
            PermissionMethod::HeldPermissions { .. } => None,
        })
    }
}
