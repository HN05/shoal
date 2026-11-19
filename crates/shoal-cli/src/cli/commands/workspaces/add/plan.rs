//! What an addition creates, checked with the daemon before anything is
//! created, and the agent arguments its launch carries.
use std::ffi::OsString;

use anyhow::Result;

use super::target::AddTarget;
use crate::{cli::context::Context, env, start_plan::Creation};
use shoal_protocol::space_names::SpaceSelector;

/// Agent arguments as a queued launch carries them, refusing one that is
/// not UTF-8.
pub(in crate::cli::commands) fn launch_args(args: Vec<OsString>) -> Result<Vec<String>> {
    (args.into_iter())
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| anyhow::anyhow!("agent argument {arg:?} is not UTF-8"))
        })
        .collect()
}

/// Refuse what creation would refuse in the given repository, branch and
/// options before a prompt asks for the rest of the addition. Only a terminal
/// prompts; without one, creation refuses the arguments itself, except that
/// a `parent`, else the chat this runs in, that the workspace cannot go below
/// is refused before a launch is queued.
pub(super) async fn check_add_creation(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
    parent: Option<&SpaceSelector>,
) -> Result<()> {
    let parent = (parent.cloned()).or_else(|| env::own_chat().map(SpaceSelector::Id));
    if !ctx.interactive() && parent.is_none() {
        return Ok(());
    }
    let planned = crate::client::workspaces::PlannedWorkspace {
        repository: target.selector.clone(),
        branch: creation.branch.clone(),
        existing: creation.existing.clone(),
        base: creation.base.clone(),
        git_profile: creation.git_profile.clone(),
        path: creation.path.clone(),
        parent,
        options: creation.options.clone(),
    };
    crate::client::workspaces::check_creation(&ctx.paths, planned).await
}
