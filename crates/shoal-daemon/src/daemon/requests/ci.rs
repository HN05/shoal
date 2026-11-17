//! The daemon's answers to [`CiMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use super::items::own_context;
use crate::protocol::{Body, CiMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: CiMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        CiMethod::CiRunJobs { target, run } => {
            let listed = manager.ci_run_jobs(&target, run, caller).await;
            return Ok(Answer::pages(listed, Body::CiRunJobs));
        }
        CiMethod::ListCiRuns { target, pr, filter } => {
            let listed = manager.list_ci_runs(&target, pr, filter, caller).await;
            return Ok(Answer::pages(listed, Body::CiRuns));
        }
        CiMethod::CheckLog {
            target,
            selection,
            check,
        } => Body::CheckLog(
            manager
                .check_log(&target, &selection, &check, caller)
                .await?,
        ),
        CiMethod::CiAction {
            target,
            item,
            action,
            checks,
        } => Body::CiActed(
            manager
                .ci_action(&target, item, action, checks, caller)
                .await?,
        ),
        CiMethod::CiRunAction {
            target,
            run,
            action,
            failed,
        } => Body::CiRunActed(
            manager
                .ci_run_action(&target, run, action, failed, caller)
                .await?,
        ),
        CiMethod::ApproveCiRuns { target, run, pr } => {
            Body::CiRunsApproved(manager.approve_ci_runs(&target, run, pr, caller).await?)
        }
        CiMethod::ViewCiRun { target, run } => {
            Body::CiRun(manager.view_ci_run(&target, run, caller).await?)
        }
        CiMethod::CiRunJobLog { target, run, job } => {
            let log = manager
                .ci_run_job_log(&target, run, job, caller)
                .await
                .and_then(|log| super::items::json_pieces(&log));
            return Ok(Answer::pages(log, Body::ViewPieces));
        }
        CiMethod::StartCiRun {
            target,
            workflow,
            branch,
            inputs,
        } => Body::CiRunStarted(
            manager
                .start_ci_run(&target, workflow, branch, inputs, caller)
                .await?,
        ),
    }))
}

impl Scoped for CiMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // Approving runs asks for the `ci_approve` permission once it
            // knows the runs it approves.
            CiMethod::ApproveCiRuns { target, .. } => {
                let context = own_context(caller, target);
                super::items::reached_change_context(manager, caller, context).await?;
                Some(super::items::target_selector(target)?)
            }
            // A scoped caller acts from its own space unless it names one
            // below, and a chat without a repository acts in a registered
            // checkout as its own space, with its agent account.
            CiMethod::CheckLog { target, .. }
            | CiMethod::CiAction { target, .. }
            | CiMethod::ListCiRuns { target, .. }
            | CiMethod::CiRunAction { target, .. }
            | CiMethod::ViewCiRun { target, .. }
            | CiMethod::CiRunJobs { target, .. }
            | CiMethod::CiRunJobLog { target, .. }
            | CiMethod::StartCiRun { target, .. } => {
                let context = own_context(caller, target);
                Some(super::items::change_context_space(manager, caller, context).await?)
            }
        })
    }
}
