//! The daemon's answers to [`AgentMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use shoal_ids::execution_ids::ExecutionId;
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
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
        AgentMethod::ListAgents => {
            let agents = manager.agents.running_agents().await?;
            Body::RunningAgents(
                manager
                    .coordination
                    .visible_agents(&**manager, caller, agents)
                    .await?,
            )
        }
        AgentMethod::SpaceAgents { space } => {
            Body::RunningAgents(manager.agents.space_agents(&**manager, &space).await?)
        }
        AgentMethod::InterruptTurn { space, execution } => Body::AgentStatus(
            manager
                .agents
                .interrupt_turn(&**manager, &space, execution)
                .await?,
        ),
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
            acknowledge_delivery(manager, &space, deliveries, messages, &leftovers).await?;
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
                    .agents
                    .report_agent_state(
                        &**manager,
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
        AgentMethod::SetProgress { space, change } => Body::Progress(
            manager
                .spaces
                .set_progress(&**manager, &space, change)
                .await?,
        ),
        AgentMethod::Progress { space } => {
            Body::Progress(manager.spaces.space_progress(&space).await?)
        }
        AgentMethod::Sleep {
            space,
            sleep,
            timer,
        } => Body::Slept(
            manager
                .agents
                .sleep(&**manager, &space, sleep, timer)
                .await?,
        ),
        AgentMethod::StartWokenTurn { workspace } => Body::WokenTurn(WokenTurn {
            starts: manager
                .agents
                .start_woken_turn(&**manager, &workspace, execution(caller))
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
                .agents
                .report_turn_failure(
                    &**manager,
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
                .agents
                .report_subagent(
                    &**manager,
                    &space,
                    execution(caller),
                    change,
                    UnixTime::now(),
                )
                .await?;
            Body::Ok
        }
        AgentMethod::ReportSubagentRequest { space, execution } => {
            manager
                .agents
                .report_subagent_request(&**manager, &space, execution, UnixTime::now())
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
                .agents
                .report_upstream_reset(&**manager, &workspace, execution, resets_at)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportUpstreamLimit {
            workspace,
            execution,
            limit,
        } => {
            manager
                .agents
                .report_upstream_limit(&**manager, &workspace, execution, limit)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportProxyRetry {
            workspace,
            execution,
            retry,
        } => {
            manager
                .agents
                .report_proxy_retry(&**manager, &workspace, execution, retry)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportInput {
            workspace,
            execution,
            at,
        } => {
            manager
                .agents
                .report_input(&**manager, &workspace, &execution, at)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportTyping {
            workspace,
            execution,
        } => {
            manager
                .agents
                .report_typing(&**manager, &workspace, &execution)
                .await?;
            Body::Ok
        }
        AgentMethod::ReportAttach { space } => {
            manager
                .agents
                .record_user_interaction(&**manager, caller, &space)
                .await;
            Body::Ok
        }
        AgentMethod::RestartAgent { space, restart } => Body::Restart(
            manager
                .agents
                .restart_space(&**manager, caller, &space, restart)
                .await?,
        ),
        AgentMethod::CancelRestart { space } => {
            Body::CancelledRestart(manager.agents.cancel_restart(&**manager, &space).await?)
        }
        AgentMethod::AgentSession { space } => Body::AgentSession(
            manager
                .agents
                .agent_session(&**manager, &space, caller.and_then(Caller::execution_id))
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

impl Checked for AgentMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::SetAgentState { .. }
            | Self::SetProgress { .. }
            | Self::Progress { .. }
            | Self::Sleep { .. }
            | Self::StartWokenTurn { .. }
            | Self::ReportTurnFailure { .. }
            | Self::ReportSubagent { .. }
            | Self::ReportSubagentRequest { .. }
            | Self::ReportWakeEnd { .. }
            | Self::ReportProxyRetry { .. }
            | Self::ReportUpstreamReset { .. }
            | Self::ReportUpstreamLimit { .. }
            | Self::ReportInput { .. }
            | Self::ReportTyping { .. }
            | Self::ReportAttach { .. }
            | Self::AwaitAgentWake { .. }
            | Self::SpaceAgents { .. }
            | Self::CancelRestart { .. }
            | Self::AgentSession { .. }
            | Self::AgentUpdates { .. }
            | Self::WaitAgentUpdates { .. }
            | Self::UpdatesWaiting { .. }
            | Self::AcknowledgePrUpdates { .. } => Some(TargetCheck::Space),
            Self::RestartAgent { .. } | Self::InterruptTurn { .. } => {
                Some(TargetCheck::RunningSpace)
            }
            Self::GitCredential { .. } | Self::ListAgents => None,
        }
    }
}

impl Scoped for AgentMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            AgentMethod::GitCredential { .. } => None,
            // The handler narrows the list to the agents below the caller's
            // space.
            AgentMethod::ListAgents => None,
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
                    .coordination.own_workspace(manager,
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
                    .coordination.own_workspace(manager,
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
                    .coordination.workspace_below(manager,
                        caller,
                        space,
                        "scoped processes interrupt and continue only the agents of the spaces below theirs",
                    )
                    .await?,
            ),
            AgentMethod::RestartAgent { space, .. } | AgentMethod::CancelRestart { space } => Some(
                manager
                    .coordination.workspace_below(manager,
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

/// Acknowledge the item updates, messages and leftovers the agent of the
/// space `selector` names received.
async fn acknowledge_delivery(
    manager: &Arc<Manager>,
    selector: &SpaceSelector,
    deliveries: Vec<String>,
    messages: Vec<i64>,
    leftovers: &[u32],
) -> Result<()> {
    // Item updates first: a failure then leaves both to be delivered
    // again, while deleted messages could not be.
    (manager.links)
        .acknowledge_pr_updates(&**manager, selector, deliveries)
        .await?;
    (manager.agents)
        .mark_agent_messages_delivered(&**manager, selector, messages)
        .await?;
    manager
        .spaces
        .acknowledge_leftovers(selector, leftovers)
        .await
}
