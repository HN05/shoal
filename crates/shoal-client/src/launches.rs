//! Agent launches: the request that starts an agent as a launch asks.
use anyhow::Result;
use shoal_protocol::launches::{Launch, Launched};
use shoal_protocol::protocol::LaunchMethod;

use crate::connection::{Daemon, request};

/// Start an agent as `launch` asks, and return where it starts once its
/// space has it.
pub async fn launch(daemon: &impl Daemon, launch: Launch) -> Result<Launched> {
    let launch = Box::new(launch);
    request(daemon, LaunchMethod::Launch { launch }).await
}

#[cfg(test)]
mod tests;
