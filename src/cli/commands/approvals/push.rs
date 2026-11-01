//! The push the user approved, in the operation approving it started: the
//! approved commit as a tracked command, as `shoal pr open` pushes, then the
//! daemon decides the request, opening the PR it proposed.
use anyhow::Result;

use crate::{
    cli::{
        client,
        commands::operations::{Failed, Reporter},
        context::Context,
    },
    daemon::approvals::{ApprovalRequest, PushSpecification},
    git,
    protocol::{
        ApprovalMethod,
        operations::{Failure, Step},
    },
};

pub(in crate::cli::commands) async fn push_approved(
    ctx: &Context,
    workspace: String,
    approval: String,
    push: PushSpecification,
    reporter: &Reporter<'_>,
) -> Result<Box<ApprovalRequest>> {
    reporter.step(Step::PushStarted).await?;
    let command = git::approved_push_command(&push);
    let failure = match crate::execution::publish(&ctx.paths, workspace, command).await {
        Ok(0) => None,
        Ok(code) => Some(format!(
            "git push of {} to {} failed (exit {code})",
            push.short_destination(),
            push.remote
        )),
        Err(error) => Some(format!("{error:#}")),
    };
    if failure.is_none() {
        reporter.reached(Step::Pushed).await?;
    }
    let finish = ApprovalMethod::FinishPush {
        key: approval,
        failure: failure.clone(),
    };
    let decided = client::request(&ctx.paths, finish).await;
    match failure {
        Some(message) => Err(Failed(Failure::PushFailed { message }).into()),
        None => decided,
    }
}
