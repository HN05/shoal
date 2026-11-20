//! A start in an existing workspace whose stopped or asleep agent it does
//! not replace: on a terminal, the user resumes that agent, replaces it with
//! the new one or cancels the start; without one, the daemon's refusal
//! stands, naming `shoal resume` and `--replace`.
use anyhow::{Context as _, Result, bail};
use shoal_protocol::launches::Launch;
use shoal_protocol::space_names::SpaceSelector;

use super::{super::StartOptions, check};
use crate::{
    cli::{args::SpaceTargets, commands::resume, context::Context, ui},
    model::Workspace,
    protocol::{ErrorCode, RemoteError},
};

/// What the daemon's check of a launch led to.
pub(super) enum Checked {
    /// The start goes on, into the existing workspace, if any.
    Goes(Option<Box<Workspace>>),
    /// The user resumed the workspace's stopped agent instead, which
    /// ended with this exit code.
    Resumed(i32),
}

/// Have the daemon check `launch` as [`check::check`] does, and where it
/// refuses the stopped agent of the workspace the start goes into, ask on
/// a terminal what to do with it: replacing it makes `launch` replace it.
pub(super) async fn checked(
    ctx: &Context,
    launch: &mut Launch,
    options: &StartOptions,
) -> Result<Checked> {
    let error = match check::check(ctx, launch, options).await {
        Ok(workspace) => return Ok(Checked::Goes(workspace.map(Box::new))),
        Err(error) => error,
    };
    if !refuses_stopped_agent(&error) || !ctx.interactive() {
        return Err(error);
    }
    let workspace = replaced_workspace(ctx, launch, options).await?;
    let label = format!("Workspace {}", workspace.number);
    match ui::stopped_agent_choice(ctx, &label)? {
        ui::StoppedAgentChoice::Resume => {
            let resumed = resume_agent(ctx, &workspace, launch, options).await?;
            Ok(Checked::Resumed(resumed))
        }
        ui::StoppedAgentChoice::Replace => {
            launch.replace = true;
            Ok(Checked::Goes(Some(Box::new(workspace))))
        }
        ui::StoppedAgentChoice::Cancel => bail!("start canceled"),
    }
}

/// Check `launch`, whose mode now starts the agent of the existing
/// workspace it goes into, as [`checked`] does where the first check left
/// the kind of space open, returning the exit code of the resume the user
/// chose instead of the start.
pub(super) async fn resumed(
    ctx: &Context,
    launch: &mut Launch,
    options: &StartOptions,
) -> Result<Option<i32>> {
    Ok(match checked(ctx, launch, options).await? {
        Checked::Goes(_) => None,
        Checked::Resumed(code) => Some(code),
    })
}

/// Whether the daemon refused a start with `error` because the workspace
/// it goes into has a stopped agent the start does not replace.
fn refuses_stopped_agent(error: &anyhow::Error) -> bool {
    (error.downcast_ref::<RemoteError>())
        .is_some_and(|remote| remote.code == ErrorCode::StoppedAgent)
}

/// The workspace whose stopped agent `launch` would replace, as the
/// daemon's check of the replacing launch names it, refusing what that
/// launch refuses, such as a scoped caller's own space.
async fn replaced_workspace(
    ctx: &Context,
    launch: &Launch,
    options: &StartOptions,
) -> Result<Workspace> {
    let replacing = Launch {
        replace: true,
        ..launch.clone()
    };
    (check::check(ctx, &replacing, options).await?)
        .context("the start goes into no existing workspace")
}

/// Resume the stopped agent of `workspace` as `shoal resume` does, at once
/// for a `launch` that starts now, or in this terminal where `options`
/// focus it.
async fn resume_agent(
    ctx: &Context,
    workspace: &Workspace,
    launch: &Launch,
    options: &StartOptions,
) -> Result<i32> {
    let targets = SpaceTargets {
        space: vec![SpaceSelector::id(&workspace.id)],
        all: false,
    };
    let resumed = resume::Options {
        execution: None,
        now: launch.now,
        focus: options.focus,
        change: None,
    };
    resume::run(ctx, targets, resumed).await
}
