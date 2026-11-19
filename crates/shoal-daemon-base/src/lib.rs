//! What every subsystem of the daemon uses: the daemon's log, who sends it
//! a request, the approval requests it makes, the global config it runs
//! under, the gates that order conflicting changes, the journal of events,
//! the notifications for the user, the numbers of questions and approval
//! requests, and the checks of file access that can stall.
use shoal_core::{config, git, refusal};
use shoal_forge_types as forge;
use shoal_protocol::{model, protocol, state, time};
use shoal_store as store;

pub mod alerts;
pub mod approval_requests;
pub mod callers;
pub mod config_state;
pub mod events;
pub mod file_access;
pub mod gates;
pub mod item_numbers;
pub mod log;
pub mod notifications;
