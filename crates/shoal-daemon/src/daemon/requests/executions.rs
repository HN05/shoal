//! The daemon's answers to [`ExecutionMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, ensure};

use super::super::{
    answer::Answer,
    execution_connection,
    executions::ExecutionKind,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::protocol::{Body, ExecutionMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ExecutionMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ExecutionMethod::Execute {
            space,
            wrapper,
            kind,
            agent,
            model,
            mode,
            narrowing,
            new_session,
            recover,
            source,
            reattach,
            queue_entry,
            demo,
            origin,
            merge,
        } => {
            // A restore the user runs in their own terminal is their
            // interaction with its agent, as one they queue is.
            if recover && source == Some(shoal_protocol::launches::LaunchSource::Manual) {
                manager.record_user_interaction(caller, &space).await;
            }
            let manager = manager.clone();
            let context = execution_connection::ExecutionContext {
                space,
                wrapper,
                kind,
                agent: agent.map(|name| shoal_protocol::executions::TrackedAgent {
                    name,
                    model,
                    mode: mode.unwrap_or(crate::config::modes::ModeName::DEFAULT),
                    narrowing,
                    new_session,
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
        ExecutionMethod::SessionSignalled { signal } => {
            (manager.runner)
                .cancel_job_for_signal(&**manager, caller, signal)
                .await?;
            Body::Ok
        }
    }))
}

impl Scoped for ExecutionMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
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
            // The caller reports its own execution's session.
            ExecutionMethod::SessionSignalled { .. } => None,
            ExecutionMethod::Execute {
                space,
                kind: ExecutionKind::Command | ExecutionKind::Merge | ExecutionKind::Unmerge,
                ..
            } => Some(space),
            ExecutionMethod::Execute {
                space,
                kind: ExecutionKind::Setup,
                ..
            } => {
                refuse_nested_setup(caller)?;
                Some(space)
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

#[cfg(test)]
mod tests;
