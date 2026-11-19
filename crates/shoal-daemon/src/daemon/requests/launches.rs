//! The daemon's answers to [`LaunchMethod`] requests, and who may send them.
use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, LaunchMethod};
use shoal_protocol::{
    launches::{LaunchSource, SpaceChoice},
    space_names::SpaceSelector,
};

pub(super) async fn handle(
    manager: &std::sync::Arc<Manager>,
    method: LaunchMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        LaunchMethod::Launch { launch } => {
            // What a client sends is a start it made itself.
            let source = LaunchSource::Manual;
            Body::Launched(manager.launch(*launch, source, caller).await?)
        }
        LaunchMethod::CheckLaunch { launch } => {
            Body::CheckedLaunch(manager.check_workspace_launch(*launch, caller).await?)
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
            LaunchMethod::Launch { launch } => {
                manager.select_launch_spaces(launch).await?;
                match &mut launch.in_workspace {
                    Some(SpaceChoice::Space(workspace)) => Some(workspace),
                    _ => None,
                }
            }
            // A check confines where its new workspace goes as the start
            // does.
            LaunchMethod::CheckLaunch { .. } => None,
        })
    }
}
