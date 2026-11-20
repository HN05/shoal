//! Resuming the stopped agents of spaces, workspaces' and chats' alike, for
//! `shoal resume` and the other commands that bring agents back: queuing
//! their restores, which the daemon starts, once an execution a daemon
//! restart left unknown is proven ended, or starting them at once beside
//! the caller's terminal, and handing the commands stopped with them to the
//! restored agent or the user. The caller passes in what it does its own
//! way through [`Resuming`].
pub mod commands;
pub mod permissions;
mod places;
pub mod queue;
pub mod sessions;
pub mod stale;

use shoal_core::execution::recovery::SavedCommand;

use crate::{client::output::batch::Failure, model::Space};

/// What a caller of a resume does its own way: telling the user what no
/// restored agent receives and what failed.
pub trait Resuming: Sync {
    /// Tell the user of the stopped `commands` of `space`, which no agent
    /// receives.
    fn stopped_commands(&self, space: &Space, commands: &[SavedCommand]);

    /// Tell the user that resuming the space named `space` failed with
    /// `error`, which the resume reports as the returned failure while it
    /// goes on with the others.
    fn failed(&self, space: &str, error: &anyhow::Error) -> Failure;
}
