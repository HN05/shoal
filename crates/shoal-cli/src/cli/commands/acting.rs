//! The request each command sends for the space it acts on, by which its picker
//! and completion judge spaces. Every command declares it in a match with no
//! default arm. A command that names a space but declares none picks only to
//! read in that space's context, and completion offers it every space.
use crate::cli::{Command, ui::Acting};

mod approvals;
mod changes;
mod coordination;
mod create;
mod integration;
mod internal;
mod items;
mod lifecycle;
mod repository;
mod resources;
mod service;
mod sessions;
mod space;
mod workspace;

/// What `command` sends for the space a picker or completion offers;
/// `None` for one that sends nothing a space's offer depends on.
pub(in crate::cli) fn acting(command: &Command) -> Option<Acting> {
    match command {
        // A run names its workspace before what it runs, which a run
        // without either lists instead.
        Command::Run { .. } => Some(super::exec::running()),
        Command::Create(command) => create::acting(command),
        Command::Items(command) => items::acting(command),
        Command::Sessions(command) => sessions::acting(command),
        Command::Coordination(command) => coordination::acting(command),
        Command::Space(command) => space::acting(command),
        Command::Workspace(command) => workspace::acting(command),
        Command::Changes(command) => changes::acting(command),
        Command::Resources(command) => resources::acting(command),
        Command::Lifecycle(command) => lifecycle::acting(command),
        Command::Internal { command } => internal::acting(command),
        Command::Service(command) => service::acting(command),
        Command::Integration(command) => integration::acting(command),
        Command::Repository(command) => repository::acting(command),
        Command::Approvals { command, .. } => approvals::acting(command.as_ref()),
    }
}
