//! A start whose agent waits for other work: the daemon records its
//! workspace at once, with the agent queued there, and prepares the
//! workspace once that work is done, on the branch of the done blocker that
//! holds the others' work.
use anyhow::Result;

use super::{
    finish::{AddStart, queued_launch},
    launch::report_workspace,
    target::AddTarget,
};
use crate::{
    cli::{commands::issues, context::Context},
    client::preparation,
    model::Blocker,
    start_plan::AddPlan,
};
use shoal_protocol::space_names::SpaceSelector;

/// Start the planned workspace with its agent waiting for `blockers`, and
/// report the workspace and the agent's entry.
pub(super) async fn start_blocked(
    ctx: &Context,
    plan: AddPlan,
    target: &AddTarget,
    issue: Option<&issues::Issue>,
    blockers: Vec<Blocker>,
) -> Result<i32> {
    let AddPlan {
        creation,
        agent,
        args,
        prompt,
        mode,
        narrowing,
        fork,
        chat,
        parent,
        workspace,
        ..
    } = plan;
    let told = (&prompt, args.as_slice(), &fork);
    let launch = queued_launch(&agent, issue, (&mode, narrowing), told, false)?;
    let start = AddStart {
        creation,
        parent: parent.or(chat.map(SpaceSelector::Id)),
        workspace,
        queue_entry: None,
        waiting: None,
        agent: launch.map(|launch| Box::new((*launch).into())),
        blockers,
    };
    let new = start.request(target, issue)?;
    let started = preparation::start(&ctx.paths, new, crate::env::without_secrets());
    report_workspace(ctx, ctx.progress("Opening workspace", started).await?)
}
