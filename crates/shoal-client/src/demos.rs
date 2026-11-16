//! Demos: named commands Shoal keeps running in a workspace so the user can
//! try a change, usually from another machine over SSH. A repository names
//! them in `[demos.NAME]`; a preset says how a demo reaches the user, and the
//! repository always supplies the command.
use shoal_protocol::demos::{Progress, Running};

use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::protocol::DemoMethod;
use shoal_protocol::space_names::SpaceSelector;

/// The demos running in `workspace`, or in every workspace.
pub async fn running(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
) -> Result<Vec<Running>> {
    request(daemon, DemoMethod::ListDemos { workspace }).await
}

/// How far `workspace`'s demo launch `launch` came.
pub async fn progress(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    launch: String,
) -> Result<Progress> {
    request(daemon, DemoMethod::DemoProgress { workspace, launch }).await
}

/// Stop `workspace`'s demo `name`, or all of them.
pub async fn stop(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: Option<String>,
) -> Result<()> {
    request(daemon, DemoMethod::StopDemos { workspace, name }).await
}
