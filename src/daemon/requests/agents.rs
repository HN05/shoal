//! The daemon's answers to [`AgentMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::{
    protocol::{AgentMethod, Body},
    state::AgentState,
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: AgentMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        AgentMethod::AwaitAgentWake { workspace, due } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                manager.wait_agent_wake(&workspace, due).await?;
                Ok(Body::Ok)
            }));
        }
        AgentMethod::AcknowledgePrUpdates {
            workspace,
            deliveries,
            messages,
        } => {
            manager
                .acknowledge_agent_delivery(&workspace, deliveries, messages)
                .await?;
            Body::Ok
        }
        AgentMethod::SetAgentState {
            workspace,
            state,
            session,
            turn_end,
        } => Body::AgentStatus(match turn_end {
            Some(turn_end) => {
                ensure!(state == AgentState::Idle, "only an idle report ends a turn");
                manager
                    .end_agent_turn(&workspace, execution(caller), session, turn_end)
                    .await?
            }
            None => {
                manager
                    .set_agent_state(&workspace, state, execution(caller), session)
                    .await?
            }
        }),
        AgentMethod::ReportTurnFailure {
            workspace,
            failure,
            session,
            execution: reported,
            turn_end,
        } => Body::AgentStatus(
            manager
                .report_turn_failure(
                    &workspace,
                    failure,
                    execution(caller).or(reported),
                    session,
                    turn_end,
                )
                .await?,
        ),
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
        AgentMethod::SendAgentMessage { workspace, message } => {
            manager.send_agent_message(&workspace, message).await?;
            Body::Ok
        }
        AgentMethod::AgentMessages { workspace } => {
            Body::AgentMessages(manager.agent_messages(&workspace).await?)
        }
        AgentMethod::AgentUpdates { workspace } => {
            Body::PrUpdates(manager.agent_updates(&workspace).await?)
        }
        AgentMethod::MarkAgentMessagesDelivered { workspace, ids } => {
            manager
                .mark_agent_messages_delivered(&workspace, ids)
                .await?;
            Body::Ok
        }
    }))
}

impl Scoped for AgentMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            AgentMethod::AwaitAgentWake { workspace, .. }
            | AgentMethod::AgentMessages { workspace }
            | AgentMethod::MarkAgentMessagesDelivered { workspace, .. }
            | AgentMethod::AgentUpdates { workspace }
            | AgentMethod::AcknowledgePrUpdates { workspace, .. } => Some(workspace),
            AgentMethod::ReportTurnFailure {
                execution: Some(_), ..
            } => bail!("workspace processes report a failed turn only for their own execution"),
            // Only the agent's wrapper, which runs its proxy, reports it.
            AgentMethod::ReportProxyRetry { .. } | AgentMethod::ReportUpstreamReset { .. } => {
                bail!("workspace processes cannot report what a proxy learns")
            }
            // A report claims the workspace for the reporting agent, which
            // would keep a child's events from reaching its coordinator.
            AgentMethod::SetAgentState { workspace, .. }
            | AgentMethod::ReportTurnFailure { workspace, .. } => Some(
                manager
                    .own_workspace(
                        caller,
                        workspace,
                        "workspace processes report agent state only for their own workspace",
                    )
                    .await?,
            ),
            AgentMethod::SendAgentMessage { workspace, .. } => Some(
                manager
                    .workspace_below(
                        caller,
                        workspace,
                        "workspace processes send agent messages only to the workspaces below theirs",
                    )
                    .await?,
            ),
        })
    }
}

/// The tracked execution a report comes from.
fn execution(caller: Option<&Caller>) -> Option<String> {
    caller.and_then(Caller::execution_id).map(str::to_owned)
}
