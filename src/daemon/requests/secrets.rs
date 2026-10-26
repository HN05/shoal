//! The daemon's answers to [`SecretMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, SecretMethod};

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
            .acquire_secret(&workspace, name, request, caller)
            .await?
            .into_body(Body::SecretGrant),
        SecretMethod::SecretRelease { workspace, name } => {
            manager.release_secret(&workspace, name).await?;
            Body::Ok
        }
        SecretMethod::SecretCatalog { workspace } => {
            Body::SecretCatalog(manager.secret_catalog(&workspace).await?)
        }
    }))
}

impl Scoped for SecretMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            SecretMethod::SecretAcquire { workspace, .. }
            | SecretMethod::SecretRelease { workspace, .. }
            | SecretMethod::SecretCatalog { workspace } => Some(workspace),
        })
    }
}
