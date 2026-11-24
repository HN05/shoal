//! Resuming the stopped agents of spaces, workspaces' and chats' alike, in
//! the caller's terminal for `shoal resume --focus`, once an execution a
//! daemon restart left unknown is proven ended, and having their space's
//! daemon restore the agents a move stopped, handing the commands stopped
//! with them to the restored agent or the user. The caller passes in what
//! it does its own way through [`Resuming`].
pub mod commands;
pub mod permissions;
mod picker;
pub mod space;
pub mod stale;
pub mod stopped;

use anyhow::Result;
use shoal_core::execution::recovery::SavedSession;

use crate::{client::output::batch::Failure, model::Space};

/// What a caller of a resume does its own way: picking the agent to
/// restore and telling the user what no restored agent receives and what
/// failed.
pub trait Resuming: Sync {
    /// The one of several stopped `agents` of a space that the user picks
    /// to restore, each named by its one of `labels`.
    fn pick_agent(&self, agents: &[SavedSession], labels: &[String]) -> Result<SavedSession>;

    /// Tell the user of the stopped `commands` of `space`, each as its
    /// arguments, which no agent receives.
    fn stopped_commands(&self, space: &Space, commands: &[Vec<String>]);

    /// Tell the user that resuming the space named `space` failed with
    /// `error`, which the resume reports as the returned failure while it
    /// goes on with the others.
    fn failed(&self, space: &str, error: &anyhow::Error) -> Failure;
}
