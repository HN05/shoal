//! The daemon's answers to [`ChatMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
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
            path: manager.prepare_chat_root().await?,
        }),
    }))
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
