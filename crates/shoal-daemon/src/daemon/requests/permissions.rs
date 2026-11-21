//! The daemon's answers to [`PermissionMethod`] requests, who may send them,
//! and what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::{Context as _, Result};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
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
                Some(repository) => Some(manager.repositories.repository(repository).await?.id),
                None => None,
            };
            let held = manager.caller_permissions(caller, repository.as_ref());
            Body::Permissions(held.await?)
        }
    }))
}

impl Checked for PermissionMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::LaunchPermissions { .. } => Some(TargetCheck::Space),
            Self::HeldPermissions { .. } => None,
        }
    }
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
