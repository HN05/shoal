//! The daemon's answers to [`CiMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    permissions::Action,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionKey,
    protocol::{Body, CiMethod, ItemContext},
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: CiMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        CiMethod::CiRunJobs { context, run } => {
            let listed = manager.ci_run_jobs(context.as_ref(), run, caller).await;
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
                .ci_run_action(context.as_ref(), run, action, failed, caller)
                .await?,
        ),
        CiMethod::ApproveCiRuns { context, run, pr } => {
            Body::CiRunsApproved(manager.approve_ci_runs(&context, run, pr, caller).await?)
        }
        CiMethod::ViewCiRun { context, run } => {
            Body::CiRun(manager.view_ci_run(context.as_ref(), run, caller).await?)
        }
        CiMethod::CiRunJobLog { context, run, job } => {
            let log = manager
                .ci_run_job_log(context.as_ref(), run, job, caller)
                .await
                .and_then(|log| super::items::json_pieces(&log));
            return Ok(Answer::pages(log, Body::ViewPieces));
        }
        CiMethod::StartCiRun {
            context,
            workflow,
            branch,
            inputs,
        } => Body::CiRunStarted(
            manager
                .start_ci_run(context.as_ref(), workflow, branch, inputs, caller)
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
            // Approving runs is the `ci_approve` permission's.
            CiMethod::ApproveCiRuns { context, run, pr } => {
                let runs = match (run.as_ref(), pr.as_ref()) {
                    (Some(run), _) => format!("CI run {run}"),
                    (None, Some(pr)) => format!("the CI runs of PR {pr}"),
                    (None, None) => "the CI runs of the space's PR".to_owned(),
                };
                let action = Action::new(PermissionKey::CiApprove, format!("approve {runs}"));
                manager.authorize_action(Some(caller), action).await?;
                Some(super::items::change_context_space(manager, caller, context).await?)
            }
            // A chat without a repository acts in a registered checkout as
            // its own space, with its agent account.
            CiMethod::CheckLog { context, .. }
            | CiMethod::CiAction { context, .. }
            | CiMethod::ListCiRuns { context, .. } => {
                Some(super::items::change_context_space(manager, caller, context).await?)
            }
            // A workspace process acts from its own workspace, so only on
            // runs of its repository.
            CiMethod::CiRunAction { context, .. }
            | CiMethod::ViewCiRun { context, .. }
            | CiMethod::CiRunJobs { context, .. }
            | CiMethod::CiRunJobLog { context, .. }
            | CiMethod::StartCiRun { context, .. } => {
                let own = own_context(manager, caller).await?;
                let context = context.get_or_insert(own);
                Some(super::items::change_context_space(manager, caller, context).await?)
            }
        })
    }
}

/// The caller's own space as the context of a request that names none.
async fn own_context(manager: &Manager, caller: &Caller) -> Result<ItemContext> {
    let space = caller.space_id.clone();
    Ok(match manager.caller_is_chat(caller).await? {
        true => ItemContext::Chat(SpaceSelector::id(&space)),
        false => ItemContext::Workspace(SpaceSelector::id(&space)),
    })
}
