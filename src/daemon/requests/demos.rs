//! The daemon's answers to [`DemoMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, DemoMethod};

pub(super) async fn handle(manager: &Arc<Manager>, method: DemoMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        DemoMethod::ListDemos { workspace } => {
            Body::Demos(manager.demos(workspace.as_deref()).await?)
        }
        DemoMethod::DemoProgress { workspace, launch } => {
            Body::DemoProgress(manager.demo_progress(&workspace, &launch).await?)
        }
        DemoMethod::StopDemos { workspace, name } => {
            manager.stop_demos(&workspace, name).await?;
            Body::Ok
        }
    }))
}

impl Scoped for DemoMethod {
    async fn scope(&mut self, _: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            DemoMethod::ListDemos { workspace } => Some(caller.own_by_default(workspace)),
            DemoMethod::DemoProgress { workspace, .. }
            | DemoMethod::StopDemos { workspace, .. } => Some(workspace),
        })
    }
}
