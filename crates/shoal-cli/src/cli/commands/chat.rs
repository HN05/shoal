//! Chats, which `shoal start` starts for a mode of their kind: an agent the
//! user talks with about a repository in its registered checkout, about a
//! workspace's work in its worktree, or without a repository in `root_dir`.
//! A chat is no workspace's agent, so it is not a tracked execution; the
//! daemon records it while its agent runs.
//! Every start makes a new chat, focused as its mode says.
mod issue;

mod agent;
mod launch;
mod list;
mod options;
mod start;
mod topic;

pub(super) use agent::{chat_agent, check_model};
pub(super) use launch::{report_started, start_changed};
pub(super) use list::{kept, list};
pub(super) use options::{ChatPlace, Narrowing, Options};
pub(super) use start::{
    Subject, root_dir, start, start_handed, start_in_workspace, start_outside_repositories,
};
pub(super) use topic::{Relaunch, Topic};
