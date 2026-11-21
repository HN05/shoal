//! The daemon's answers to [`ChatMethod`] requests, who may send them, and what
//! each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, ChatMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ChatMethod,
    _caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ChatMethod::ChatRoot => Body::ChatRoot(crate::protocol::ChatRoot {
            path: manager.repositories.prepare_chat_root().await?,
        }),
    }))
}

impl Checked for ChatMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::ChatRoot => None,
        }
    }
}

impl Scoped for ChatMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            ChatMethod::ChatRoot => None,
        })
    }
}
