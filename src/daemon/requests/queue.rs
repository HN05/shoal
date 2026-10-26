//! The daemon's answers to [`QueueMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::super::{
    agent_queue,
    answer::Answer,
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::protocol::{Body, QueueMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: QueueMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        QueueMethod::QueueHold { entry } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                agent_queue::hold(stream, request_id, manager, entry, guard)
            }));
        }
        QueueMethod::QueueAdd { request } => {
            Body::QueueAdmission(manager.queue_add(request).await?)
        }
        QueueMethod::ListQueue => Body::QueueEntries(if caller.is_some() {
            Vec::new()
        } else {
            manager.queue_entries().await?
        }),
        QueueMethod::QueuedPlan { entry } => Body::QueuedPlan(manager.queued_plan(&entry).await?),
        QueueMethod::QueueRemove { entry } => Body::QueueEntry(manager.queue_remove(&entry).await?),
        QueueMethod::QueueAttach { entry, workspace } => {
            manager.queue_attach(&entry, &workspace).await?;
            Body::Ok
        }
        QueueMethod::QueueFail { entry, error } => {
            manager.fail_queue_entry(&entry, &error).await?;
            Body::Ok
        }
    }))
}

impl Scoped for QueueMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            QueueMethod::ListQueue => None,
            QueueMethod::QueueAdd { request } => {
                let denied =
                    "workspace processes can start agents only in the workspaces split from theirs";
                let Some(target) = request.workspace.as_mut() else {
                    bail!(denied);
                };
                ensure!(
                    manager.workspace(target).await?.id != caller.workspace_id,
                    denied
                );
                Some(target)
            }
            QueueMethod::QueuedPlan { .. }
            | QueueMethod::QueueRemove { .. }
            | QueueMethod::QueueHold { .. }
            | QueueMethod::QueueAttach { .. }
            | QueueMethod::QueueFail { .. } => return Err(denied()),
        })
    }
}
