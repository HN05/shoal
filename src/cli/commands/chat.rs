//! Chats, which `shoal start` starts for a mode of their kind: an agent the
//! user talks with about a repository in its registered checkout, about a
//! workspace's work in its worktree, or without a repository in `root_dir`.
//! A chat is no workspace's agent, so it is not a tracked execution; the
//! daemon records it while its agent runs.
//! Every start makes a new chat and gives it focus.
pub(super) mod handoff;
mod issue;
pub(super) mod removal;
mod rename;

mod agent;
mod launch;
mod list;
mod options;
mod start;
mod topic;

pub(super) use agent::{chat_agent, check_model, checked_agent};
pub(super) use launch::{Started, launch};
pub(super) use list::{list, listed};
pub(super) use options::{ChatPlace, Narrowing, Options, ensure_scope_token};
pub(super) use rename::{ask_name, rename, request_name};
pub(super) use start::{
    Subject, hosting_workspace, root_dir, settings, start, start_in_workspace,
    start_outside_repositories,
};
pub(super) use topic::{Relaunch, Topic};
