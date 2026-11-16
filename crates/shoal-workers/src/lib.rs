//! The execution wrapper and the workers: the code that runs in the
//! processes Shoal starts and calls the daemon. Core's `workers` module
//! builds those processes' command lines for the daemon, which never
//! depends on this crate.

pub mod git_credential;
pub mod push_gate;
pub mod secrets;

use shoal_client as client;
use shoal_core::{env, fsutil, git, tools};
use shoal_protocol::{model, protocol};

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
#[cfg(test)]
shoal_core::forget_runner_repository!();
