//! The daemon's answers to [`LaunchMethod`] requests, and who may send them.
use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, LaunchMethod};
use shoal_protocol::{launches::SpaceChoice, space_names::SpaceSelector};

pub(super) async fn handle(
    manager: &std::sync::Arc<Manager>,
    method: LaunchMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        LaunchMethod::Launch { launch, source } => {
            Body::Launched(manager.launch(*launch, source, caller).await?)
        }
    }))
}

impl Scoped for LaunchMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // A scoped caller starts agents only in its own tree, in the
            // existing workspace it names as in the new spaces it creates,
            // which the launch puts below its own space or one below it.
            LaunchMethod::Launch { launch, .. } => {
                manager.select_launch_spaces(launch).await?;
                match &mut launch.in_workspace {
                    Some(SpaceChoice::Space(workspace)) => Some(workspace),
                    _ => None,
                }
            }
        })
    }
}
