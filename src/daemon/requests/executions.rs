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
            model,
            recover,
            reattach,
            queue_entry,
            demo,
            origin,
            merge,
        } => {
            let manager = manager.clone();
            let context = execution_connection::ExecutionContext {
                workspace,
                wrapper,
                kind,
                agent: agent.map(|name| crate::execution::TrackedAgent { name, model }),
                recover,
                reattach,
                queue_entry,
                demo: demo.map(|demo| *demo),
                origin: origin.map(|origin| *origin),
                merge: merge.map(|merge| *merge),
                caller: caller.cloned(),
            };
            return Ok(Answer::connection(move |stream, request_id, guard| {
                execution_connection::execute(stream, request_id, manager, context, guard)
            }));
        }
        ExecutionMethod::CheckMerge => {
            require_merging(caller, "merge execution required")?;
            Body::Ok
        }
        ExecutionMethod::ListAgents => Body::RunningAgents(manager.running_agents().await?),
        ExecutionMethod::RestartAgent {
            execution,
            join,
            run,
        } => {
            if let Some(joined) = join {
                manager.join_restart(&execution, joined.run, run).await?;
            } else {
                manager.restart_agent(&execution, run).await?;
            }
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
            ExecutionMethod::CheckMerge => {
                require_merging(
                    Some(caller),
                    "only an authorized merge execution may run the merge worker",
                )?;
                None
            }
            ExecutionMethod::Execute {
                workspace,
                kind: ExecutionKind::Command | ExecutionKind::Merge,
                ..
            } => Some(workspace),
            ExecutionMethod::Execute {
                workspace,
                kind: ExecutionKind::Setup,
                ..
            } => {
                refuse_nested_setup(caller)?;
                Some(workspace)
            }
        })
    }
}

/// Refuse a caller other than a merge execution; `refusal` says why.
fn require_merging(caller: Option<&Caller>, refusal: &str) -> Result<()> {
    ensure!(
        caller.is_some_and(|caller| caller.kind() == Some(ExecutionKind::Merge)),
        "{refusal}"
    );
    Ok(())
}

/// Refuse a setup command running setup.
fn refuse_nested_setup(caller: &Caller) -> Result<()> {
    ensure!(
        caller.kind() != Some(ExecutionKind::Setup),
        "a setup command cannot recursively run setup"
    );
    Ok(())
}
