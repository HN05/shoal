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
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(Some(match self {
            HoldMethod::HoldList { workspace } | HoldMethod::HoldRelease { workspace, .. } => {
                workspace
            }
            HoldMethod::HoldAcquire { workspace, .. } => {
                manager
                    .workspace_below(
                        caller,
                        workspace,
                        "workspace processes hold only the workspaces below theirs; ask with shoal questions ask",
                    )
                    .await?
            }
        }))
    }
}
