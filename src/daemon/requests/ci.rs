//! The daemon's answers to [`CiMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, CiMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: CiMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        CiMethod::CiRunJobs { workspace, run } => {
            let listed = manager.ci_run_jobs(&workspace, run).await;
            return Ok(Answer::pages(listed, Body::CiRunJobs));
        }
        CiMethod::ListCiRuns { workspace, filter } => {
            let listed = manager.list_ci_runs(&workspace, filter).await;
            return Ok(Answer::pages(listed, Body::CiRuns));
        }
        CiMethod::CheckLog {
            workspace,
            selection,
            check,
        } => Body::CheckLog(manager.check_log(&workspace, &selection, &check).await?),
        CiMethod::CiAction {
            workspace,
            item,
            action,
            checks,
        } => Body::CiActed(manager.ci_action(&workspace, item, action, checks).await?),
        CiMethod::CiRunAction {
            workspace,
            run,
            action,
            failed,
        } => Body::CiRunActed(
            manager
                .ci_run_action(&workspace, run, action, failed, caller.is_some())
                .await?,
        ),
        CiMethod::ViewCiRun { workspace, run } => {
            Body::CiRun(manager.view_ci_run(&workspace, run).await?)
        }
        CiMethod::CiRunJobLog {
            workspace,
            run,
            job,
        } => Body::CheckLog(manager.ci_run_job_log(&workspace, run, job).await?),
        CiMethod::StartCiRun {
            workspace,
            workflow,
            branch,
            inputs,
        } => Body::CiRunStarted(
            manager
                .start_ci_run(&workspace, workflow, branch, inputs)
                .await?,
        ),
    }))
}

impl Scoped for CiMethod {
    async fn scope(&mut self, manager: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            CiMethod::CheckLog { workspace, .. }
            | CiMethod::CiAction { workspace, .. }
            | CiMethod::CiRunAction { workspace, .. }
            | CiMethod::ViewCiRun { workspace, .. }
            | CiMethod::CiRunJobs { workspace, .. }
            | CiMethod::CiRunJobLog { workspace, .. }
            | CiMethod::ListCiRuns { workspace, .. } => Some(workspace),
            CiMethod::StartCiRun {
                workspace, branch, ..
            } => {
                manager.own_branch(workspace, branch.as_deref()).await?;
                Some(workspace)
            }
        })
    }
}
