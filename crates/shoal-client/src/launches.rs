//! Agent launches: the request that starts an agent as a launch asks.
use anyhow::Result;
use shoal_protocol::launches::{Launch, LaunchSource, Launched};
use shoal_protocol::protocol::LaunchMethod;

use crate::connection::{Daemon, request};

/// Start an agent as `launch` asks, which `source` made, and return where
/// it starts once its space has it.
pub async fn launch(
    daemon: &impl Daemon,
    launch: Launch,
    source: LaunchSource,
) -> Result<Launched> {
    let launch = Box::new(launch);
    request(daemon, LaunchMethod::Launch { launch, source }).await
}
