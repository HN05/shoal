//! The daemon's answers to [`CiMethod`] requests, who may send them, and what
//! each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
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
            let listed = manager
                .links
                .ci_run_jobs(&**manager, &target, run, caller)
                .await;
            return Ok(Answer::pages(listed, Body::CiRunJobs));
        }
        CiMethod::ListCiRuns { target, of, filter } => {
            let listed = manager
                .links
                .list_ci_runs(&**manager, &target, of, filter, caller)
                .await;
            return Ok(Answer::pages(listed, Body::CiRuns));
        }
        CiMethod::CheckLog {
            target,
            selection,
            check,
        } => Body::CheckLog(
            (manager.links)
                .check_log(&**manager, &target, &selection, &check, caller)
                .await?,
        ),
        CiMethod::CiAction {
            target,
            item,
            action,
            checks,
        } => Body::CiActed(
            manager
                .links
                .ci_action(&**manager, &target, item, action, checks, caller)
                .await?,
        ),
        CiMethod::CiRunAction {
            target,
            run,
            action,
            failed,
        } => Body::CiRunActed(
            manager
                .links
                .ci_run_action(&**manager, &target, run, action, failed, caller)
                .await?,
        ),
        CiMethod::ApproveCiRuns { target, run, pr } => Body::CiRunsApproved(
            manager
                .links
                .approve_ci_runs(&**manager, &target, run, pr, caller)
                .await?,
        ),
        CiMethod::ViewCiRun { target, run } => Body::CiRun(
            manager
                .links
                .view_ci_run(&**manager, &target, run, caller)
                .await?,
        ),
        CiMethod::CiRunJobLog { target, run, job } => {
            let log = manager
                .links
                .ci_run_job_log(&**manager, &target, run, job, caller)
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
                .links
                .start_ci_run(&**manager, &target, workflow, branch, inputs, caller)
                .await?,
        ),
    }))
}

impl Checked for CiMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::CheckLog { .. }
            | Self::CiAction { .. }
            | Self::CiRunAction { .. }
            | Self::ViewCiRun { .. }
            | Self::CiRunJobs { .. }
            | Self::CiRunJobLog { .. }
            | Self::ListCiRuns { .. }
            | Self::ApproveCiRuns { .. }
            | Self::StartCiRun { .. } => Some(TargetCheck::Space),
        }
    }
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
