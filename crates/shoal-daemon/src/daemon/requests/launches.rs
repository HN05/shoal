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
            Body::CheckedLaunch(manager.check_launch(*launch, caller).await?)
        }
        LaunchMethod::CheckCreation { launch } => {
            Body::CheckedLaunch(manager.check_creation_launch(*launch, caller).await?)
        }
        LaunchMethod::LaunchModes { launch } => {
            Body::LaunchModes(manager.launch_modes(&launch, caller).await?)
        }
        LaunchMethod::TakeParentRepository { launch } => {
            manager
                .take_launch_parent_repository(*launch, caller)
                .await?;
            Body::Ok
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
            // which the launch puts below its own space or one below it; a
            // check confines them as the start does.
            LaunchMethod::Launch { launch }
            | LaunchMethod::CheckLaunch { launch }
            | LaunchMethod::CheckCreation { launch } => {
                manager.select_launch_spaces(launch).await?;
                match &mut launch.in_workspace {
                    Some(SpaceChoice::Space(workspace)) => Some(workspace),
                    _ => None,
                }
            }
            // Registering the parent's repository checks the parent against
            // the caller's scope first.
            LaunchMethod::LaunchModes { .. } | LaunchMethod::TakeParentRepository { .. } => None,
        })
    }
}
