//! CLI workspace workflows; all state mutations go through the daemon.
mod add;
mod adopt;
mod cd;
mod changes;
mod done;
mod environment;
mod link_pull;
mod list;
mod merge;
mod open;
mod prepare;
mod recovery;
mod removal;
mod remove;
mod rename;
mod run;
mod setup;
mod status;
mod title;
mod unmerge;
mod watch;

pub(super) use add::{AddOptions, AgentLaunch, add};
pub(in crate::cli) use add::{AddPlan, CopyCreation, Creation, execute_add, start_queued};
pub(crate) use add::{check_queued_launch, queued_agent_request};
pub(super) use adopt::adopt;
pub(super) use cd::cd;
pub(super) use changes::{conflicts, diff};
pub(super) use done::{Done, done, undone};
pub(in crate::cli) use environment::agent_environment;
pub(super) use environment::environment;
pub(super) use link_pull::link_pull;
pub(super) use list::list;
pub(super) use merge::{merge, merge_branch, merge_worker};
pub(super) use open::open;
pub(super) use prepare::prepare;
pub(super) use removal::{ensure_batch_removable, leaving};
pub(super) use remove::remove;
pub(super) use rename::rename;
pub(super) use run::{edit, exec};
pub(super) use setup::{
    SetUp, copy_ignored_worker, follow_setup, run_post_setup, setup, setup_workspace, skip_setup,
};
pub(super) use status::status;
pub(super) use title::{request_title, title};
pub(super) use unmerge::{unmerge, unmerge_branch, unmerge_worker};
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
