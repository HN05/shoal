//! The daemon's answers to [`QueueMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    queued_starts,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, QueueMethod};
use crate::state::QueueKind;
use shoal_protocol::space_names::SpaceSelector;

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
        } => {
            let entry = manager.queue_agent(&workspace, *launch, blockers, pr);
            Body::QueueEntry(entry.await?)
        }
        QueueMethod::QueueChat { request } => {
            Body::QueueAdmission(manager.queue_chat_add(*request, caller).await?)
        }
        QueueMethod::ListQueue => Body::QueueEntries(manager.visible_queue().await?),
        QueueMethod::QueuedPlan { entry } => Body::QueuedPlan(manager.queued_plan(&entry).await?),
        QueueMethod::QueueRemove { entry } => {
            Body::QueueEntry(manager.queue_remove(&entry, caller).await?)
        }
        QueueMethod::QueueStart { entry, here } => {
            let entry = manager.queue_start(&entry, here).await?;
            if entry.kind == QueueKind::Restore
                && let Some(space) = entry.space()
            {
                (manager.record_user_interaction(caller, &SpaceSelector::id(space))).await;
            }
            Body::QueueEntry(entry)
        }
        QueueMethod::QueueClaim { entry } => Body::QueueEntry(manager.queue_claim(&entry).await?),
        QueueMethod::StartedLaunch { entry } => {
            Body::StartedLaunch(manager.started_launch(&entry).await?)
        }
        QueueMethod::QueueAttach { entry, space } => {
            manager.queue_attach(&entry, &space).await?;
            Body::Ok
        }
        QueueMethod::QueueFail { entry, error } => {
            manager.fail_queue_entry(&entry, &error).await?;
            Body::Ok
        }
        QueueMethod::QueueRetry { entry, here } => {
            let entry = manager.retry_failed_launch(&entry, here).await?;
            // A restore the user runs in their own terminal is their
            // interaction with its agent.
            if here && let Some(space) = entry.space() {
                (manager.record_user_interaction(caller, &SpaceSelector::id(space))).await;
            }
            Body::QueueEntry(entry)
        }
        QueueMethod::SetPriority { space, priority } => {
            Body::Prioritized(manager.set_priority(caller, &space, priority).await?)
        }
        QueueMethod::QueueRestore {
            workspace,
            execution,
            env,
            now,
        } => {
            manager.record_user_interaction(caller, &workspace).await;
            Body::QueueEntry(match now {
                false => (manager.queue_stopped_restore(&workspace, &execution, env)).await?,
                true => (manager.start_stopped_restore(&workspace, &execution, env)).await?,
            })
        }
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
            QueueMethod::QueueChat { request } => {
                manager.confine_chat_launch(caller, request).await?;
                None
            }
            QueueMethod::SetPriority { space, .. } => Some(space),
            QueueMethod::QueueAgent {
                workspace, launch, ..
            } => {
                let (mode, given) = (launch.mode.as_ref(), &launch.narrowing);
                let target = manager.worker_target(caller, workspace, mode, given);
                let target = target.await?;
                if launch.now {
                    (manager.authorize_start_beyond_limits(caller, "start", target)).await?;
                }
                Some(target)
            }
            QueueMethod::QueueRestore { workspace, now, .. } => {
                let target = manager.stopped_below(caller, workspace).await?;
                if *now {
                    (manager.authorize_start_beyond_limits(caller, "restore", target)).await?;
                }
                Some(target)
            }
            // A space's own failed launch restores as its stopped agent
            // does: only from the spaces above it.
            QueueMethod::QueueRetry { entry, .. } => {
                manager.ensure_entry_reached(caller, entry).await?;
                None
            }
            QueueMethod::QueueStart { entry, .. } => {
                let id = entry.clone().into();
                let repository = manager.reached_entry(caller, &id).await?.repository_id;
                let start = format!("start queue entry {entry} now");
                (manager.authorize_beyond_limits(caller, start, repository)).await?;
                None
            }
            // A caller holds, claims, reads, fails and attaches the launches
            // it starts itself, which its scope reaches.
            QueueMethod::QueueHold { entry }
            | QueueMethod::QueueClaim { entry }
            | QueueMethod::QueuedPlan { entry }
            | QueueMethod::QueueFail { entry, .. } => {
                manager.ensure_entry_reached(caller, entry).await?;
                None
            }
            QueueMethod::QueueAttach { entry, space } => {
                manager.ensure_entry_reached(caller, entry).await?;
                Some(space)
            }
            QueueMethod::StartedLaunch { entry } => {
                manager.ensure_started_launch_reached(caller, entry).await?;
                None
            }
        })
    }
}
