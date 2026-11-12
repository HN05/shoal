//! The push the user approved, in the operation approving it started: the
//! approved commit as a tracked command, as `shoal pr open` pushes, then the
//! daemon decides the request, opening the PR it proposed.
use anyhow::Result;

use super::{Failed, Reporter};
use crate::{
    client::{
        approvals::{ApprovalRequest, PushSpecification},
        space_names::SpaceSelector,
    },
    git,
    paths::CommandPaths,
    protocol::operations::{Failure, Step},
};

pub(super) async fn push_approved(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    approval: String,
    push: PushSpecification,
    reporter: &Reporter<'_>,
) -> Result<Box<ApprovalRequest>> {
    reporter.step(Step::PushStarted).await?;
    let command = git::approved_push_command(&push);
    let failure = match crate::execution::publish(paths, workspace, command).await {
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
    let decided = crate::client::approvals::finish_push(paths, approval, failure.clone()).await;
    match failure {
        Some(message) => Err(Failed(Failure::PushFailed { message }).into()),
        None => decided,
    }
}
