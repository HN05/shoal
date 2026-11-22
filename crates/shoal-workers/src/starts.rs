//! Carrying out a start the daemon recorded, in the process that runs its
//! agent: following a new workspace's preparation, hosting the space in the
//! place opened for it, and running its agent once the queue lets it, as
//! `shoal start` here and the place a start opens beside a terminal both
//! do. What the process reports its own way is its [`Starting`] frontend.
mod placed;
mod prepared;
mod run;

use anyhow::Result;

pub use placed::{Attach, Placed, execute_place, run};
pub use prepared::{enter_workspace, finish_workspace, follow_preparation, shell_in, unprepared};
pub use run::run_entry;

use crate::{hosting::Caller, model::Workspace, preparation::Frontend};

/// What a process that carries out a start does its own way, besides
/// following preparations as its [`Frontend`] does.
pub trait Starting: Frontend {
    /// What hosting reads of the process.
    fn caller(&self) -> Caller<'_>;

    /// Show `workspace`, which its preparation readied, as created, or as
    /// opened when it was `reused`.
    fn prepared(&self, workspace: &Workspace, reused: bool) -> Result<()>;

    /// Tell a user that the shell that started the process cannot follow
    /// it into a workspace without Shoal's shell integration.
    fn not_integrated(&self);
}
