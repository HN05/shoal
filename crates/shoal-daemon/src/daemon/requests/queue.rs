//! The daemon's answers to [`QueueMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    queued_starts,
    scope::{Caller, Scoped, denied},
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
            } => {
                let (mode, given) = (launch.mode.as_ref(), &launch.narrowing);
                let target = manager.worker_target(caller, workspace, mode, given);
                let target = target.await?;
                if launch.now {
                    let start = format!("start the agent of {target} now");
                    manager.authorize_beyond_limits(caller, start).await?;
                }
                Some(target)
            }
            QueueMethod::QueueRestore { workspace, now, .. } => {
                let target = manager.stopped_below(caller, workspace).await?;
                if *now {
                    let start = format!("restore the agent of {target} now");
                    manager.authorize_beyond_limits(caller, start).await?;
                }
                Some(target)
            }
            QueueMethod::QueueStart { entry, .. } => {
                manager
                    .ensure_entry_reached(caller, &entry.clone().into())
                    .await?;
                let start = format!("start queue entry {entry} now");
                manager.authorize_beyond_limits(caller, start).await?;
                None
            }
            // A caller holds, fails and attaches the launches it starts
            // itself, which its scope reaches.
            QueueMethod::QueueHold { entry } | QueueMethod::QueueFail { entry, .. } => {
                manager.ensure_entry_reached(caller, entry).await?;
                None
            }
            QueueMethod::QueueAttach { entry, workspace } => {
                manager.ensure_entry_reached(caller, entry).await?;
                Some(workspace)
            }
            QueueMethod::StartedLaunch { entry } => {
                manager.ensure_started_launch_reached(caller, entry).await?;
                None
            }
            QueueMethod::QueuedPlan { .. } => {
                return Err(denied());
            }
        })
    }
}
