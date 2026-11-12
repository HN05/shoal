//! The daemon's answers to [`QueueMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, ensure};

use super::super::{
    answer::Answer,
    queued_starts,
    scope::{Caller, NOT_NOW, Scoped, denied},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
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
                queued_starts::hold(stream, request_id, manager, entry, guard)
            }));
        }
        QueueMethod::QueueAgent {
            workspace,
            launch,
            blockers,
            pr,
        } => Body::QueueEntry(
            manager
                .queue_agent(&workspace, *launch, blockers, pr)
                .await?,
        ),
        QueueMethod::QueueAdd { request } => {
            Body::QueueAdmission(manager.queue_add_as(*request, caller).await?)
        }
        QueueMethod::ListQueue => Body::QueueEntries(manager.visible_queue().await?),
        QueueMethod::QueuedPlan { entry } => Body::QueuedPlan(manager.queued_plan(&entry).await?),
        QueueMethod::QueueRemove { entry } => {
            Body::QueueEntry(manager.queue_remove(&entry, caller).await?)
        }
        QueueMethod::QueueStart { entry, here } => {
            Body::QueueEntry(manager.queue_start(&entry, here).await?)
        }
        QueueMethod::StartedLaunch { entry } => {
            Body::StartedLaunch(manager.started_launch(&entry).await?)
        }
        QueueMethod::QueueAttach { entry, workspace } => {
            manager.queue_attach(&entry, &workspace).await?;
            Body::Ok
        }
        QueueMethod::QueueFail { entry, error } => {
            manager.fail_queue_entry(&entry, &error).await?;
            Body::Ok
        }
        QueueMethod::SetPriority { space, priority } => {
            Body::Prioritized(manager.set_priority(caller, &space, priority).await?)
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
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // Each answer confines the entries to those below the caller.
            QueueMethod::ListQueue | QueueMethod::QueueRemove { .. } => None,
            QueueMethod::QueueAdd { request } => manager.queue_target(caller, request).await?,
            QueueMethod::SetPriority { space, .. } => Some(space),
            QueueMethod::QueueAgent {
                workspace, launch, ..
            } => Some(manager.worker_target(caller, workspace, launch).await?),
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
            | QueueMethod::StartedLaunch { .. }
            | QueueMethod::QueueHold { .. }
            | QueueMethod::QueueAttach { .. }
            | QueueMethod::QueueFail { .. } => return Err(denied()),
        })
    }
}
