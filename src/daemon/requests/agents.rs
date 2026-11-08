//! The daemon's answers to [`AgentMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail, ensure};

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    protocol::{AgentMethod, Body, WokenTurn},
    state::AgentState,
};

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
                    .wait_agent_wake(&workspace, due, hooked.as_deref())
                    .await?;
                Ok(Body::Ok)
            }));
        }
        AgentMethod::WorkspaceAgents { workspace } => {
            Body::RunningAgents(manager.workspace_agents(&workspace).await?)
        }
        AgentMethod::InterruptTurn {
            workspace,
            execution,
        } => Body::AgentStatus(manager.interrupt_turn(&workspace, execution).await?),
        AgentMethod::WaitAgentUpdates { space } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                Ok(Body::PrUpdates(manager.wait_space_updates(&space).await?))
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
        AgentMethod::SetProgress { space, change } => {
            Body::Progress(manager.set_progress(&space, change).await?)
        }
        AgentMethod::Progress { space } => Body::Progress(manager.space_progress(&space).await?),
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
        AgentMethod::ReportWakeEnd { space, ended } => {
            manager.report_wake_end(&space, &ended).await?;
            Body::Ok
        }
        AgentMethod::ReportStateFailure { workspace, error } => {
            manager.report_state_failure(&workspace, &error).await?;
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
        AgentMethod::ReportAttached {
            workspace,
            execution,
            attached,
        } => {
            manager
                .report_attached(&workspace, &execution, attached)
                .await?;
            Body::Ok
        }
        AgentMethod::AgentSession { workspace } => Body::AgentSession(
            manager
                .agent_session(&workspace, caller.and_then(Caller::execution_id))
                .await?,
        ),
        AgentMethod::SendAgentMessage {
            space,
            message,
            attachments,
            sender_chat,
        } => {
            let attacher = manager.attacher(caller, sender_chat.as_ref()).await;
            manager
                .send_space_message(&space, message, attachments, &attacher)
                .await?;
            Body::Ok
        }
        AgentMethod::SendAllAgentMessage {
            repository,
            message,
            attachments,
            sender_chat,
        } => {
            let attacher = manager.attacher(caller, sender_chat.as_ref()).await;
            let below = manager.reach_root(caller).await?;
            let below = below.as_ref();
            Body::Reached(
                manager
                    .send_all_message(
                        repository.as_ref(),
                        message,
                        attachments,
                        &attacher,
                        below,
                        sender_chat.as_ref(),
                    )
                    .await?,
            )
        }
        AgentMethod::AgentMessages { space } => {
            Body::AgentMessages(manager.space_messages(&space).await?)
        }
        AgentMethod::AgentUpdates { space } => {
            Body::PrUpdates(manager.space_updates(&space).await?)
        }
        AgentMethod::MarkAgentMessagesDelivered { space, ids } => {
            manager.mark_space_messages_delivered(&space, ids).await?;
            Body::Ok
        }
    }))
}

impl Scoped for AgentMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            AgentMethod::AwaitAgentWake { workspace, .. }
            | AgentMethod::AgentSession { workspace } => Some(workspace),
            AgentMethod::AgentMessages { space }
            | AgentMethod::MarkAgentMessagesDelivered { space, .. }
            | AgentMethod::AgentUpdates { space }
            | AgentMethod::WaitAgentUpdates { space }
            | AgentMethod::ReportWakeEnd { space, .. }
            | AgentMethod::Progress { space }
            | AgentMethod::AcknowledgePrUpdates { space, .. } => Some(space),
            AgentMethod::ReportTurnFailure {
                execution: Some(_), ..
            } => bail!("workspace processes report a failed turn only for their own execution"),
            // Only the agent's wrapper, which runs its proxy, reports it.
            AgentMethod::ReportProxyRetry { .. } | AgentMethod::ReportUpstreamReset { .. } => {
                bail!("workspace processes cannot report what a proxy learns")
            }
            // Only the agent's wrapper reads the place the agent runs in.
            AgentMethod::ReportAttached { .. } => {
                bail!("workspace processes cannot report whether a terminal shows an agent")
            }
            // A report claims the workspace for the reporting agent, which
            // would keep a child's events from reaching its coordinator.
            AgentMethod::SetAgentState { workspace, .. }
            | AgentMethod::StartWokenTurn { workspace }
            | AgentMethod::ReportTurnFailure { workspace, .. }
            | AgentMethod::ReportStateFailure { workspace, .. } => Some(
                manager
                    .own_workspace(
                        caller,
                        workspace,
                        "workspace processes report agent state only for their own workspace",
                    )
                    .await?,
            ),
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
            AgentMethod::WorkspaceAgents { workspace }
            | AgentMethod::InterruptTurn { workspace, .. } => Some(
                manager
                    .workspace_below(
                        caller,
                        workspace,
                        "workspace processes interrupt and continue only the agents of the workspaces below theirs",
                    )
                    .await?,
            ),
            AgentMethod::SendAgentMessage { space: workspace, .. } => Some(
                manager
                    .workspace_below(
                        caller,
                        workspace,
                        "workspace processes send agent messages only to the workspaces below theirs",
                    )
                    .await?,
            ),
            // The handler narrows the recipients to the workspaces below
            // the caller's.
            AgentMethod::SendAllAgentMessage {
                repository: Some(repository),
                ..
            } => {
                *repository = manager
                    .own_repository(
                        caller,
                        repository,
                        "workspace processes send agent messages only to the workspaces below theirs",
                    )
                    .await?;
                None
            }
            AgentMethod::SendAllAgentMessage {
                repository: None, ..
            } => None,
        })
    }
}

/// The tracked execution a report comes from.
fn execution(caller: Option<&Caller>) -> Option<String> {
    caller.and_then(Caller::execution_id).map(str::to_owned)
}
