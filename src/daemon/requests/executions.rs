//! The daemon's answers to [`ExecutionMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, ensure};

use super::super::{
    answer::Answer,
    execution_connection,
    scope::{Caller, Scoped},
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
            mode,
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
                agent: agent.map(|name| crate::execution::TrackedAgent {
                    name,
                    model,
                    mode: mode.unwrap_or_else(|| crate::config::modes::DEFAULT.to_owned()),
                }),
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
        ExecutionMethod::ListAgents => {
            let agents = manager.running_agents().await?;
            Body::RunningAgents(manager.visible_agents(caller, agents).await?)
        }
        ExecutionMethod::RestartAgent {
            execution,
            join,
            run,
            in_place,
        } => Body::RestartedPlace(match join {
            Some(joined) => manager.join_restart(&execution, joined.run, run).await?,
            None => manager.restart_agent(&execution, run, in_place).await?,
        }),
    }))
}

impl Scoped for ExecutionMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            // The handler narrows the list to the agents below the caller's
            // space, the ones it may restart.
            ExecutionMethod::ListAgents => None,
            ExecutionMethod::RestartAgent { execution, .. } => {
                manager.ensure_agent_below(caller, execution).await?;
                None
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
