//! The daemon's answers to [`CiMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::{Result, bail};

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, CiMethod, ItemContext};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: CiMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        CiMethod::CiRunJobs { context, run } => {
            let listed = manager
                .ci_run_jobs(context.as_ref(), run, caller.is_some())
                .await;
            return Ok(Answer::pages(listed, Body::CiRunJobs));
        }
        CiMethod::ListCiRuns {
            context,
            pr,
            filter,
        } => {
            let listed = manager.list_ci_runs(&context, pr, filter).await;
            return Ok(Answer::pages(listed, Body::CiRuns));
        }
        CiMethod::CheckLog {
            context,
            selection,
            check,
        } => Body::CheckLog(manager.check_log(&context, &selection, &check).await?),
        CiMethod::CiAction {
            context,
            item,
            action,
            checks,
        } => Body::CiActed(manager.ci_action(&context, item, action, checks).await?),
        CiMethod::CiRunAction {
            context,
            run,
            action,
            failed,
        } => Body::CiRunActed(
            manager
                .ci_run_action(context.as_ref(), run, action, failed, caller.is_some())
                .await?,
        ),
        CiMethod::ApproveCiRuns { context, run, pr } => {
            Body::CiRunsApproved(manager.approve_ci_runs(&context, run, pr).await?)
        }
        CiMethod::ViewCiRun { context, run } => Body::CiRun(
            manager
                .view_ci_run(context.as_ref(), run, caller.is_some())
                .await?,
        ),
        CiMethod::CiRunJobLog { context, run, job } => Body::CheckLog(
            manager
                .ci_run_job_log(context.as_ref(), run, job, caller.is_some())
                .await?,
        ),
        CiMethod::StartCiRun {
            context,
            workflow,
            branch,
            inputs,
        } => Body::CiRunStarted(
            manager
                .start_ci_run(context.as_ref(), workflow, branch, inputs, caller.is_some())
                .await?,
        ),
    }))
}

impl Scoped for CiMethod {
    async fn scope(&mut self, _: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            CiMethod::ApproveCiRuns { .. } => {
                bail!("approving CI runs is the user's decision; ask them with shoal alerts send")
            }
            CiMethod::CheckLog { context, .. }
            | CiMethod::CiAction { context, .. }
            | CiMethod::ListCiRuns { context, .. } => {
                Some(super::items::context_workspace(context)?)
            }
            // A workspace process acts from its own workspace, so only on
            // runs of its repository.
            CiMethod::CiRunAction { context, .. }
            | CiMethod::ViewCiRun { context, .. }
            | CiMethod::CiRunJobs { context, .. }
            | CiMethod::CiRunJobLog { context, .. }
            | CiMethod::StartCiRun { context, .. } => {
                let context = context.get_or_insert_with(|| own_context(caller));
                Some(super::items::context_workspace(context)?)
            }
        })
    }
}

/// The caller's own workspace as the context of a request that names none.
fn own_context(caller: &Caller) -> ItemContext {
    ItemContext::Workspace(caller.workspace_id.clone())
}
