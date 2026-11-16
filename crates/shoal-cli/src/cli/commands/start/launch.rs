//! Launching the agent of a new space: a chat's, through the daemon, in an
//! app or a place of its own, or here, and a new workspace's that the
//! daemon starts without the caller's terminal.
mod chat;
pub(super) mod workspace;

pub(in crate::cli::commands) use chat::{begin, report_started, start_changed};
