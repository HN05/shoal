//! The daemon's answers to [`ConversionMethod`] requests, who may send them,
//! and what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    conversions::WorkspaceConversion,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
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
                    .spaces
                    .convert_to_workspace(&manager.daemon(), &chat, conversion, env, caller)
                    .await?,
            )
        }
        ConversionMethod::ConvertToChat { workspace, mode } => Body::Workspace(
            manager
                .spaces
                .convert_to_chat(
                    &**manager,
                    &workspace,
                    mode.unwrap_or_else(crate::model::chat_mode),
                )
                .await?,
        ),
        ConversionMethod::ConvertAgent { space, mode, agent } => Body::AgentSwitch(Box::new(
            manager
                .spaces
                .convert_agent(&**manager, &space, mode, agent, caller)
                .await?,
        )),
    }))
}

impl Checked for ConversionMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::ConvertToWorkspace { .. } => Some(TargetCheck::Space),
            Self::ConvertToChat { .. } => Some(TargetCheck::ReadyWorkspace),
            Self::ConvertAgent { .. } => Some(TargetCheck::Space),
        }
    }
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
            }
            | ConversionMethod::ConvertAgent { space, .. } => Some(space),
        })
    }
}
