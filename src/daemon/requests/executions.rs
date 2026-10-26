//! The daemon's answers to [`ExecutionMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, ensure};

use super::super::{
    answer::Answer,
    execution_connection,
    scope::{Caller, Scoped, denied},
    workspace::{ExecutionKind, Manager},
};
use crate::protocol::{Body, ExecutionMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ExecutionMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ExecutionMethod::Execute {
            workspace,
            wrapper,
            kind,
            agent,
            recover,
            reattach,
            queue_entry,
            demo,
        } => {
            let manager = manager.clone();
            let context = execution_connection::ExecutionContext {
                workspace,
                wrapper,
                kind,
                agent,
                recover,
                reattach,
                queue_entry,
                demo: demo.map(|demo| *demo),
                caller: caller.cloned(),
            };
            return Ok(Answer::connection(move |stream, request_id, guard| {
                execution_connection::execute(stream, request_id, manager, context, guard)
            }));
        }
        ExecutionMethod::CheckLanding => {
            ensure!(
                caller.is_some_and(|caller| caller.kind() == Some(ExecutionKind::Land)),
                "landing execution required"
            );
            Body::Ok
        }
        ExecutionMethod::ListAgents => Body::RunningAgents(manager.running_agents().await?),
        ExecutionMethod::RestartAgent { execution } => {
            manager.restart_agent(&execution).await?;
            Body::Ok
        }
    }))
}

impl Scoped for ExecutionMethod {
    async fn scope(&mut self, _: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            ExecutionMethod::ListAgents | ExecutionMethod::RestartAgent { .. } => {
                return Err(denied());
            }
            ExecutionMethod::CheckLanding => {
                ensure!(
                    caller.kind() == Some(ExecutionKind::Land),
                    "only an authorized landing execution may run the land worker"
                );
                None
            }
            ExecutionMethod::Execute {
                workspace,
                kind: ExecutionKind::Command | ExecutionKind::Land,
                ..
            } => Some(workspace),
            ExecutionMethod::Execute {
                workspace,
                kind: ExecutionKind::Setup,
                ..
            } => {
                ensure!(
                    caller.kind() != Some(ExecutionKind::Setup),
                    "a setup command cannot recursively run setup"
                );
                Some(workspace)
            }
        })
    }
}
