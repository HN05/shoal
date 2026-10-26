//! The daemon's answers to [`HoldMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail};

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, HoldMethod};

pub(super) async fn handle(manager: &Arc<Manager>, method: HoldMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        HoldMethod::HoldAcquire {
            workspace,
            name,
            reason,
        } => Body::Hold(manager.acquire_hold(&workspace, name, reason).await?),
        HoldMethod::HoldRelease { workspace, name } => {
            manager.release_hold(&workspace, name).await?;
            Body::Ok
        }
        HoldMethod::HoldList { workspace } => {
            Body::Holds(manager.workspace(&workspace).await?.holds)
        }
    }))
}

impl Scoped for HoldMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            HoldMethod::HoldList { workspace } => Some(workspace),
            HoldMethod::HoldAcquire { .. } | HoldMethod::HoldRelease { .. } => {
                bail!(
                    "holds belong to the user and host apps; workspace processes ask with shoal questions ask"
                )
            }
        })
    }
}
