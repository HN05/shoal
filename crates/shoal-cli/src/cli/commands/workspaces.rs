//! CLI workspace workflows; all state mutations go through the daemon.
mod add;
mod adopt;
mod branch_rename;
mod cd;
mod changes;
mod done;
mod environment;
mod list;
mod merge;
mod recovery;
mod refresh;
mod removal;
mod remove;
mod run;
mod setup;
mod status;
mod unmerge;
mod waiting;
mod watch;

pub(in crate::cli) use add::execute_add;
pub(super) use add::{
    AddOptions, AgentLaunch, DaemonReads, StartsAsChat, add, check_target, daemon_starts,
    launch_args, launch_workspace, leaves_caller, link, start_worker,
};
pub(super) use adopt::adopt;
pub(super) use branch_rename::rename_branch;
pub(super) use cd::cd;
pub(super) use changes::{conflicts, diff};
pub(super) use done::{done, undone};
pub(in crate::cli) use environment::agent_environment;
pub(super) use list::list;
pub(super) use merge::merge;
pub(super) use refresh::refresh;
pub(super) use removal::{ensure_batch_removable, leaving};
pub(super) use remove::remove;
pub(super) use run::{edit, exec};
pub(super) use setup::{SetUp, follow_setup, run_post_setup, setup, setup_workspace, skip_setup};
pub(super) use status::status;
pub(super) use unmerge::unmerge;
pub(super) use waiting::awaits_setup;
pub(super) use watch::watch_items;

/// Where the workspace's worktree is, for messages: `on branch <branch> at
/// <path>`, or `at <commit> in <path>` with what a workspace at a commit was
/// made for.
pub(super) fn location_phrase(workspace: &crate::model::Workspace) -> String {
    let path = workspace.path.display();
    match (&workspace.branch, &workspace.at) {
        (Some(branch), _) => format!("on branch {branch} at {path}"),
        (None, Some(at)) => format!("at {} in {path}", at.label()),
        (None, None) => format!("at a commit in {path}"),
    }
}
