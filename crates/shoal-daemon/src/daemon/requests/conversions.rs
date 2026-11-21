//! The daemon's answers to [`ConversionMethod`] requests, and who may send
//! them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    conversions::WorkspaceConversion,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, ConversionMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ConversionMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ConversionMethod::ConvertToWorkspace {
            chat,
            branch,
            issue,
            base,
            options,
            mode,
            env,
        } => {
            // Its preparation runs in an operation, whose worker an exit
            // would cut off.
            manager.gates.admit()?;
            let conversion = WorkspaceConversion {
                branch,
                issue,
                base,
                options,
                mode: mode.unwrap_or(crate::config::modes::ModeName::DEFAULT),
            };
            Body::WorkspaceStarted(
                manager
                    .convert_to_workspace(&chat, conversion, env, caller)
                    .await?,
            )
        }
        ConversionMethod::ConvertToChat { workspace, mode } => Body::Workspace(
            manager
                .convert_to_chat(&workspace, mode.unwrap_or_else(crate::model::chat_mode))
                .await?,
        ),
    }))
}

/// A scoped caller converts the spaces its scope reaches.
impl Scoped for ConversionMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ConversionMethod::ConvertToWorkspace { chat: space, .. }
            | ConversionMethod::ConvertToChat {
                workspace: space, ..
            } => Some(space),
        })
    }
}
