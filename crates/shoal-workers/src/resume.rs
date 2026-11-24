//! Resuming the stopped agents of spaces, workspaces' and chats' alike, in
//! the caller's terminal for `shoal resume --focus`, and queuing the
//! restores of the agents a move stopped, which the daemon starts, once an
//! execution a daemon restart left unknown is proven ended, handing the
//! commands stopped with them to the restored agent or the user. The caller
//! passes in what it does its own way through [`Resuming`].
pub mod commands;
pub mod permissions;
mod picker;
pub mod queue;
pub mod space;
pub mod stale;

use anyhow::Result;
use shoal_core::execution::recovery::{SavedCommand, SavedSession};

use crate::{client::output::batch::Failure, model::Space};

/// What a caller of a resume does its own way: picking the agent to
/// restore and telling the user what no restored agent receives and what
/// failed.
pub trait Resuming: Sync {
    /// The one of several stopped `agents` of a space that the user picks
    /// to restore, each named by its one of `labels`.
    fn pick_agent(&self, agents: &[SavedSession], labels: &[String]) -> Result<SavedSession>;

    /// Tell the user of the stopped `commands` of `space`, which no agent
    /// receives.
    fn stopped_commands(&self, space: &Space, commands: &[SavedCommand]);

    /// Tell the user that resuming the space named `space` failed with
    /// `error`, which the resume reports as the returned failure while it
    /// goes on with the others.
    fn failed(&self, space: &str, error: &anyhow::Error) -> Failure;
}
