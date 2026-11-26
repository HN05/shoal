//! The daemon code that more than one of its subsystems uses.
use shoal_core::{config, git, refusal};
use shoal_forge_types as forge;
use shoal_protocol::{model, protocol, state, time};
use shoal_store as store;

pub mod alerts;
pub mod allocation;
pub mod approval_requests;
pub mod blockers;
pub mod callers;
pub mod config_state;
pub mod containers;
pub mod events;
pub mod failures;
pub mod file_access;
pub mod file_copies;
pub mod forge_access;
pub mod frames;
#[cfg(target_os = "macos")]
pub mod frameworks;
pub mod gates;
pub mod item_numbers;
pub mod jobs;
pub mod log;
pub mod merges;
pub mod notifications;
pub mod operations;
pub mod ownership;
pub mod permitted_actions;
pub mod retention;
pub mod root_moves;
pub mod run_usage;
pub mod signals;
pub mod stops;
pub mod writable;
