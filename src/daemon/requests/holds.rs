//! The daemon's answers to [`HoldMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::{
    model::HoldPlacer,
    protocol::{Body, HoldMethod},
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: HoldMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    // A workspace process acts for its agent, anyone else for the user.
    let placer = match caller {
        Some(_) => HoldPlacer::Agent,
        None => HoldPlacer::User,
    };
    Ok(Answer::Body(match method {
        // An agent asks to hold its own workspace.
        HoldMethod::HoldAcquire {
            workspace,
            name,
            reason,
        } if caller.is_some_and(|caller| caller.workspace_id == workspace) => manager
            .request_hold(&workspace, name, reason)
            .await?
            .into_body(Body::Hold),
        HoldMethod::HoldAcquire {
            workspace,
            name,
            reason,
        } => Body::Hold(
            manager
                .acquire_hold(&workspace, name, reason, placer)
                .await?,
        ),
        HoldMethod::HoldRelease { workspace, name } => {
            manager.release_hold(&workspace, name, placer).await?;
            Body::Ok
        }
        HoldMethod::HoldList { workspace } => Body::Holds(manager.holds(&workspace).await?),
    }))
}

impl Scoped for HoldMethod {
    /// Holding the caller's own workspace needs approval, which
    /// [`handle`] requests.
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(Some(match self {
            HoldMethod::HoldList { workspace }
            | HoldMethod::HoldRelease { workspace, .. }
            | HoldMethod::HoldAcquire { workspace, .. } => workspace,
        }))
    }
}
