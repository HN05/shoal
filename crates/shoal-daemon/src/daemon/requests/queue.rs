//! The daemon's answers to [`QueueMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    queued_starts,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, QueueMethod};
use crate::state::QueueKind;
use shoal_protocol::protocol::RetryStart;
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
                queued_starts::hold(stream, request_id, manager.agents.clone(), entry, guard)
            }));
        }
        QueueMethod::QueueAgent {
            workspace,
            launch,
            blockers,
            pr,
        } => {
            let entry = manager
                .agents
                .queue_agent(&**manager, &workspace, *launch, blockers, pr);
            Body::QueueEntry(entry.await?)
        }
        QueueMethod::ListQueue => {
            Body::QueueEntries(manager.coordination.visible_queue(&**manager).await?)
        }
        QueueMethod::QueuedPlan { entry } => {
            Body::QueuedPlan(manager.agents.queued_plan(&entry).await?)
        }
        QueueMethod::OpenStart { entry } => Body::QueuedPlan(
            manager
                .agents
                .open_queued_start(&**manager, &entry, caller)
                .await?,
        ),
        QueueMethod::QueueRemove { entry } => Body::QueueEntry(
            manager
                .agents
                .queue_remove(&**manager, &entry, caller)
                .await?,
        ),
        QueueMethod::QueueStart { entry, here } => {
            let entry = manager.agents.queue_start(&**manager, &entry, here).await?;
            if entry.kind == QueueKind::Restore
                && let Some(space) = entry.space()
            {
                (manager.agents.record_user_interaction(
                    &**manager,
                    caller,
                    &SpaceSelector::id(space),
                ))
                .await;
            }
            Body::QueueEntry(entry)
        }
        QueueMethod::QueueClaim { entry } => {
            Body::QueueEntry(manager.agents.queue_claim(&**manager, &entry).await?)
        }
        QueueMethod::StartedLaunch { entry } => {
            Body::StartedLaunch(manager.agents.started_launch(&entry).await?)
        }
        QueueMethod::QueueAttach { entry, space } => {
            manager
                .agents
                .queue_attach(&**manager, &entry, &space)
                .await?;
            Body::Ok
        }
        QueueMethod::QueueFail { entry, error } => {
            manager
                .agents
                .fail_queue_entry(&**manager, &entry, &error)
                .await?;
            Body::Ok
        }
        QueueMethod::QueueRetry { entry, start } => {
            let entry = manager
                .agents
                .retry_failed_launch(&**manager, &entry, start)
                .await?;
            // A restore the user makes is their interaction with its agent,
            // as a stopped agent's is.
            if let Some(space) = entry.space() {
                (manager.agents.record_user_interaction(
                    &**manager,
                    caller,
                    &SpaceSelector::id(space),
                ))
                .await;
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
            manager
                .agents
                .record_user_interaction(&**manager, caller, &workspace)
                .await;
            Body::QueueEntry(match now {
                false => {
                    (manager
                        .agents
                        .queue_stopped_restore(&**manager, &workspace, &execution, env))
                    .await?
                }
                true => {
                    (manager
                        .agents
                        .start_stopped_restore(&**manager, &workspace, &execution, env))
                    .await?
                }
            })
        }
    }))
}

impl Checked for QueueMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::SetPriority { .. } | Self::QueueRestore { .. } => Some(TargetCheck::Space),
            Self::QueueAgent { .. } => Some(TargetCheck::Workspace),
            Self::ListQueue
            | Self::QueuedPlan { .. }
            | Self::OpenStart { .. }
            | Self::QueueRemove { .. }
            | Self::QueueStart { .. }
            | Self::QueueClaim { .. }
            | Self::StartedLaunch { .. }
            | Self::QueueHold { .. }
            | Self::QueueAttach { .. }
            | Self::QueueFail { .. }
            | Self::QueueRetry { .. } => None,
        }
    }
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
            QueueMethod::SetPriority { space, .. } => Some(space),
            QueueMethod::QueueAgent {
                workspace, launch, ..
            } => {
                let (mode, given) = (launch.mode.as_ref(), &launch.narrowing);
                let target = manager
                    .coordination
                    .worker_target(manager, caller, workspace, mode, given);
                let target = target.await?;
                if launch.now {
                    (manager
                        .coordination
                        .authorize_start_beyond_limits(manager, caller, "start", target))
                    .await?;
                }
                Some(target)
            }
            QueueMethod::QueueRestore { workspace, now, .. } => {
                let target = manager
                    .coordination
                    .stopped_below(manager, caller, workspace)
                    .await?;
                if *now {
                    (manager
                        .coordination
                        .authorize_start_beyond_limits(manager, caller, "restore", target))
                    .await?;
                }
                Some(target)
            }
            // A space's own failed launch restores as its stopped agent
            // does: only from the spaces above it.
            QueueMethod::QueueRetry { entry, start } => {
                let repository = manager
                    .coordination
                    .reached_entry(manager, caller, entry)
                    .await?
                    .repository_id;
                if *start == RetryStart::Now {
                    let start = format!("start queue entry {entry} now");
                    (manager
                        .coordination
                        .authorize_beyond_limits(manager, caller, start, repository))
                    .await?;
                }
                None
            }
            QueueMethod::QueueStart { entry, .. } => {
                let id = entry.clone().into();
                let repository = manager
                    .coordination
                    .reached_entry(manager, caller, &id)
                    .await?
                    .repository_id;
                let start = format!("start queue entry {entry} now");
                (manager
                    .coordination
                    .authorize_beyond_limits(manager, caller, start, repository))
                .await?;
                None
            }
            // A caller holds, claims, reads, opens, fails and attaches the
            // launches it starts itself, which its scope reaches.
            QueueMethod::QueueHold { entry }
            | QueueMethod::QueueClaim { entry }
            | QueueMethod::QueuedPlan { entry }
            | QueueMethod::OpenStart { entry }
            | QueueMethod::QueueFail { entry, .. } => {
                manager
                    .coordination
                    .ensure_entry_reached(manager, caller, entry)
                    .await?;
                None
            }
            QueueMethod::QueueAttach { entry, space } => {
                manager
                    .coordination
                    .ensure_entry_reached(manager, caller, entry)
                    .await?;
                Some(space)
            }
            QueueMethod::StartedLaunch { entry } => {
                manager
                    .coordination
                    .ensure_started_launch_reached(manager, caller, entry)
                    .await?;
                None
            }
        })
    }
}

#[cfg(test)]
mod tests;
