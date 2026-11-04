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
            require_kind(caller, ExecutionKind::Merge, "merge execution required")?;
            Body::Ok
        }
        ExecutionMethod::CheckUnmerge => {
            require_kind(caller, ExecutionKind::Unmerge, "unmerge execution required")?;
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
                require_kind(
                    Some(caller),
                    ExecutionKind::Merge,
                    "only an authorized merge execution may run the merge worker",
                )?;
                None
            }
            ExecutionMethod::CheckUnmerge => {
                require_kind(
                    Some(caller),
                    ExecutionKind::Unmerge,
                    "only an authorized unmerge execution may run the unmerge worker",
                )?;
                None
            }
            ExecutionMethod::Execute {
                workspace,
                kind: ExecutionKind::Command | ExecutionKind::Merge | ExecutionKind::Unmerge,
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

/// Refuse a caller other than an execution of `kind`; `refusal` says why.
fn require_kind(caller: Option<&Caller>, kind: ExecutionKind, refusal: &str) -> Result<()> {
    ensure!(
        caller.is_some_and(|caller| caller.kind() == Some(kind)),
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
