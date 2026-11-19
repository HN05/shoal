//! CLI workspace workflows; all state mutations go through the daemon.
mod add;
mod adopt;
mod branch_rename;
mod cd;
mod changes;
mod environment;
mod recovery;
mod refresh;
mod removal;
mod run;
mod setup;
mod unmerge;
mod waiting;

pub(super) use add::{
    AddOptions, DaemonReads, Opened, StartsAsChat, check_start_arguments, check_target,
    enter_created, follow_preparation, launch_args, launch_workspace, open_recorded,
    pick_start_source, reopen_with_shell, report_launched, shell_in, start_worker, unprepared,
};
pub(super) use adopt::adopt;
pub(super) use branch_rename::rename_branch;
pub(super) use cd::cd;
pub(super) use changes::{conflicts, diff};
pub(in crate::cli) use environment::agent_environment;
pub(super) use refresh::refresh;
pub(super) use removal::{ensure_batch_removable, leaving};
pub(super) use run::{edit, exec};
pub(super) use setup::{SetUp, follow_setup, run_post_setup, setup, setup_workspace, skip_setup};
pub(super) use unmerge::unmerge;
pub(super) use waiting::awaits_setup;

/// Where the workspace's worktree is, for messages: `on branch <branch> at
/// <path>`, or `at <commit> in <path>` with what a workspace at a commit was
/// made for.
pub(super) fn location_phrase(workspace: &crate::model::Workspace) -> String {
    use crate::model::Head;
    let path = workspace.path.display();
    match &workspace.head {
        Head::Branch(branch) => format!("on branch {branch} at {path}"),
        Head::Commit(at) => format!("at {} in {path}", at.label()),
    }
}
