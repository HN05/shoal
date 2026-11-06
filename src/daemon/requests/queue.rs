//! The daemon's answers to [`QueueMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, ensure};

use super::super::{
    agent_queue,
    answer::Answer,
    scope::{Caller, NOT_NOW, Scoped, denied},
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
        QueueMethod::QueueAgent {
            workspace,
            agent,
            prompt,
        } => Body::QueueEntry(manager.queue_agent(&workspace, agent, prompt).await?),
        QueueMethod::QueueAdd { request } => {
            Body::QueueAdmission(manager.queue_add(request).await?)
        }
        QueueMethod::ListQueue => Body::QueueEntries(manager.visible_queue(caller).await?),
        QueueMethod::QueuedPlan { entry } => Body::QueuedPlan(manager.queued_plan(&entry).await?),
        QueueMethod::QueueRemove { entry } => {
            Body::QueueEntry(manager.queue_remove(&entry, caller).await?)
        }
        QueueMethod::QueueStart { entry } => Body::QueueEntry(manager.queue_start(&entry).await?),
        QueueMethod::QueueMove { entry, place } => {
            Body::MovedEntry(manager.queue_move(&entry, place, caller).await?)
        }
        QueueMethod::QueueAttach { entry, workspace } => {
            manager.queue_attach(&entry, &workspace).await?;
            Body::Ok
        }
        QueueMethod::QueueFail { entry, error } => {
            manager.fail_queue_entry(&entry, &error).await?;
            Body::Ok
        }
        QueueMethod::QueueRestore {
            workspace,
            execution,
            env,
            now: false,
        } => Body::QueueEntry(
            manager
                .queue_stopped_restore(&workspace, &execution, env)
                .await?,
        ),
        QueueMethod::QueueRestore {
            workspace,
            execution,
            env,
            now: true,
        } => Body::QueueEntry(
            manager
                .start_stopped_restore(&workspace, &execution, env)
                .await?,
        ),
    }))
}

impl Scoped for QueueMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            // Each answer confines the entries to those below the caller.
            QueueMethod::ListQueue
            | QueueMethod::QueueRemove { .. }
            | QueueMethod::QueueMove { .. } => None,
            QueueMethod::QueueAdd { request } => Some(manager.queue_target(caller, request).await?),
            QueueMethod::QueueAgent { workspace, .. } => {
                const REFUSAL: &str =
                    "workspace processes can start agents only in the workspaces split from theirs";
                Some(manager.workspace_below(caller, workspace, REFUSAL).await?)
            }
            QueueMethod::QueueRestore {
                workspace,
                execution,
                now,
                ..
            } => {
                ensure!(!*now, NOT_NOW);
                Some(manager.stopped_below(caller, workspace, execution).await?)
            }
            QueueMethod::QueuedPlan { .. }
            | QueueMethod::QueueStart { .. }
            | QueueMethod::QueueHold { .. }
            | QueueMethod::QueueAttach { .. }
            | QueueMethod::QueueFail { .. } => return Err(denied()),
        })
    }
}
