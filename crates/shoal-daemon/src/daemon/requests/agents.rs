//! The daemon's answers to [`AgentMethod`] requests, and who may send them.
use shoal_ids::execution_ids::ExecutionId;
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::{
    protocol::{AgentMethod, Body, WaitingUpdates, WokenTurn},
    state::AgentState,
    time::UnixTime,
};
use shoal_protocol::progress::ProgressChange;
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: AgentMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        AgentMethod::AwaitAgentWake {
            workspace,
            due,
            hooked,
        } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                manager
                    .wait_agent_wake(&workspace, due, hooked.as_ref())
                    .await?;
                Ok(Body::Ok)
            }));
        }
        AgentMethod::SpaceAgents { space } => {
            Body::RunningAgents(manager.space_agents(&space).await?)
        }
        AgentMethod::InterruptTurn { space, execution } => {
            Body::AgentStatus(manager.interrupt_turn(&space, execution).await?)
        }
        AgentMethod::WaitAgentUpdates { space } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                Ok(Body::PrUpdates(
                    manager.links.wait_space_updates(&*manager, &space).await?,
                ))
            }));
        }
        AgentMethod::AcknowledgePrUpdates {
            space,
            deliveries,
            messages,
            leftovers,
        } => {
            manager
                .acknowledge_space_delivery(&space, deliveries, messages, &leftovers)
                .await?;
            Body::Ok
        }
        AgentMethod::SetAgentState {
            workspace,
            state,
            session,
            turn_end,
            made,
        } => {
            ensure!(
                turn_end.is_none() || state == AgentState::Idle,
                "only an idle report ends a turn"
            );
            Body::AgentStatus(
                manager
                    .report_agent_state(
                        &workspace,
                        state,
                        execution(caller),
                        session,
                        turn_end,
                        made,
                    )
                    .await?,
            )
        }
        AgentMethod::SetProgress { space, change } => {
            Body::Progress(manager.set_progress(&space, change).await?)
        }
        AgentMethod::Progress { space } => Body::Progress(manager.space_progress(&space).await?),
        AgentMethod::Sleep {
            space,
            sleep,
            timer,
        } => Body::Slept(manager.sleep(&space, sleep, timer).await?),
        AgentMethod::StartWokenTurn { workspace } => Body::WokenTurn(WokenTurn {
            starts: manager
                .start_woken_turn(&workspace, execution(caller))
                .await?,
        }),
        AgentMethod::ReportTurnFailure {
            workspace,
            failure,
            session,
            execution: reported,
            turn_end,
            made,
        } => Body::AgentStatus(
            manager
                .report_turn_failure(
                    &workspace,
                    failure,
                    execution(caller).or(reported),
                    session,
                    turn_end,
                    made,
                )
                .await?,
        ),
        AgentMethod::ReportSubagent { space, change } => {
            manager
                .report_subagent(&space, execution(caller), change, UnixTime::now())
                .await?;
            Body::Ok
        }
        AgentMethod::ReportSubagentRequest { space, execution } => {
            manager
                .report_subagent_request(&space, execution, UnixTime::now())
                .await?;
            Body::Ok
        }
        AgentMethod::ReportWakeEnd { space, ended } => {
            manager
                .links
                .report_wake_end(&**manager, &space, &ended)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportUpstreamReset {
            workspace,
            execution,
            resets_at,
        } => {
            manager
                .report_upstream_reset(&workspace, execution, resets_at)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportUpstreamLimit {
            workspace,
            execution,
            limit,
        } => {
            manager
                .report_upstream_limit(&workspace, execution, limit)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportProxyRetry {
            workspace,
            execution,
            retry,
        } => {
            manager
                .report_proxy_retry(&workspace, execution, retry)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportInput {
            workspace,
            execution,
            at,
        } => {
            manager.report_input(&workspace, &execution, at).await?;
            Body::Ok
        }
        AgentMethod::ReportTyping {
            workspace,
            execution,
        } => {
            manager.report_typing(&workspace, &execution).await?;
            Body::Ok
        }
        AgentMethod::ReportAttach { space } => {
            manager.record_user_interaction(caller, &space).await;
            Body::Ok
        }
        AgentMethod::RestartAgent { space, restart } => {
            Body::Restart(manager.restart_space(caller, &space, restart).await?)
        }
        AgentMethod::CancelRestart { space } => {
            Body::CancelledRestart(manager.cancel_restart(&space).await?)
        }
        AgentMethod::AgentSession { space } => Body::AgentSession(
            manager
                .agent_session(&space, caller.and_then(Caller::execution_id))
                .await?,
        ),
        AgentMethod::GitCredential { host } => {
            Body::GitCredential(manager.agent_git_credential(caller, &host).await?)
        }
        AgentMethod::AgentUpdates { space } => {
            Body::PrUpdates(manager.links.agent_updates(&**manager, &space).await?)
        }
        AgentMethod::UpdatesWaiting { space } => Body::WaitingUpdates(WaitingUpdates {
            waiting: manager.updates_waiting(&space).await?,
        }),
    }))
}

impl Scoped for AgentMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            AgentMethod::GitCredential { .. } => None,
            AgentMethod::AwaitAgentWake { workspace, .. } => Some(workspace),
            AgentMethod::AgentSession { space }
            | AgentMethod::AgentUpdates { space }
            | AgentMethod::UpdatesWaiting { space }
            | AgentMethod::WaitAgentUpdates { space }
            | AgentMethod::ReportWakeEnd { space, .. }
            | AgentMethod::Progress { space }
            | AgentMethod::Sleep { space, .. }
            | AgentMethod::AcknowledgePrUpdates { space, .. } => Some(space),
            AgentMethod::ReportTurnFailure {
                execution: Some(_), ..
            } => bail!("workspace processes report a failed turn only for their own execution"),
            // Only the agent's wrapper, which runs its proxy, reports it.
            AgentMethod::ReportProxyRetry { .. }
            | AgentMethod::ReportSubagentRequest { .. }
            | AgentMethod::ReportUpstreamReset { .. }
            | AgentMethod::ReportUpstreamLimit { .. } => {
                bail!("workspace processes cannot report what a proxy learns")
            }
            // Only the agent's wrapper reads the keys of its terminal.
            AgentMethod::ReportInput { .. } => {
                bail!("workspace processes cannot report the keys an agent reads")
            }
            // Only the user attaches; a scoped report counts for nothing.
            AgentMethod::ReportAttach { space } => Some(space),
            // A report claims the workspace for the reporting agent, which
            // would keep a child's events from reaching its coordinator.
            AgentMethod::SetAgentState { workspace, .. }
            | AgentMethod::ReportSubagent {
                space: workspace, ..
            }
            | AgentMethod::StartWokenTurn { workspace }
            | AgentMethod::ReportTurnFailure { workspace, .. } => Some(
                manager
                    .own_workspace(
                        caller,
                        workspace,
                        "workspace processes report agent state only for their own workspace",
                    )
                    .await?,
            ),
            // A coordinator removes what blocks a space below its own.
            AgentMethod::SetProgress {
                space,
                change: ProgressChange::RemoveBlocker { .. },
            } => Some(space),
            AgentMethod::SetProgress { space, .. } => Some(
                manager
                    .own_workspace(
                        caller,
                        space,
                        "scoped processes set progress only for their own space",
                    )
                    .await?,
            ),
            // The CLI types into the places of the agents it lists, which
            // it interrupts or continues.
            AgentMethod::SpaceAgents { space }
            | AgentMethod::InterruptTurn { space, .. }
            | AgentMethod::ReportTyping {
                workspace: space, ..
            } => Some(
                manager
                    .workspace_below(
                        caller,
                        space,
                        "scoped processes interrupt and continue only the agents of the spaces below theirs",
                    )
                    .await?,
            ),
            AgentMethod::RestartAgent { space, .. } | AgentMethod::CancelRestart { space } => Some(
                manager
                    .workspace_below(
                        caller,
                        space,
                        "scoped processes restart only the agents of the spaces below their own",
                    )
                    .await?,
            ),
        })
    }
}

/// The tracked execution a report comes from.
fn execution(caller: Option<&Caller>) -> Option<ExecutionId> {
    caller.and_then(Caller::execution_id).cloned()
}
