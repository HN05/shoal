//! The daemon's answers to [`SecretMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, SecretMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: SecretMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        SecretMethod::SecretAcquire {
            workspace,
            name,
            request,
        } => manager
            .leases
            .acquire_secret(&**manager, &workspace, name, request, caller)
            .await?
            .into_body(Body::SecretGrant),
        SecretMethod::SecretRelease { workspace, name } => {
            manager
                .leases
                .release_secret(&**manager, &workspace, name)
                .await?;
            Body::Ok
        }
        SecretMethod::SecretCatalog { workspace } => Body::SecretCatalog(
            manager
                .leases
                .secret_catalog(&**manager, &workspace)
                .await?,
        ),
    }))
}

impl Checked for SecretMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::SecretRelease { .. } => Some(TargetCheck::ReadyWorkspace),
            Self::SecretAcquire { .. } | Self::SecretCatalog { .. } => Some(TargetCheck::Workspace),
        }
    }
}

impl Scoped for SecretMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            SecretMethod::SecretAcquire { workspace, .. }
            | SecretMethod::SecretRelease { workspace, .. }
            | SecretMethod::SecretCatalog { workspace } => Some(workspace),
        })
    }
}
