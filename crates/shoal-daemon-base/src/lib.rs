//! What every subsystem of the daemon uses: the daemon's log, who sends it
//! a request, the approval requests it makes and the actions permissions
//! govern, the global config it runs under, the gates that order
//! conflicting changes, the journal of events, the failures of a pass by
//! the spaces they concern, the signals that wake waits once what they
//! read changes, the notifications for the user, the numbers of threads
//! and approval requests, a CI job's cost and what stopped its runner, how long what they record stays, the checks of
//! file access that can stall and of directories the daemon writes, and
//! the proof of the directories Shoal owns.
use shoal_core::{config, git, refusal};
use shoal_forge_types as forge;
use shoal_protocol::{model, protocol, state, time};
use shoal_store as store;

pub mod alerts;
pub mod approval_requests;
pub mod callers;
pub mod config_state;
pub mod events;
pub mod failures;
pub mod file_access;
pub mod file_copies;
#[cfg(target_os = "macos")]
pub mod frameworks;
pub mod gates;
pub mod item_numbers;
pub mod jobs;
pub mod log;
pub mod notifications;
pub mod ownership;
pub mod permitted_actions;
pub mod retention;
pub mod signals;
pub mod writable;
