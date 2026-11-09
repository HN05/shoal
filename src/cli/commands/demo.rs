//! `shoal demo`: commands kept running in a workspace for the user to try.
//! An agent starts one, the user opens it, and both can read its screen.
mod list;
mod lookup;
mod open;
mod presets;
mod screen;
mod start;
mod stop;

use anyhow::{Result, ensure};

use crate::cli::{args::sessions::DemoCommand, context::Context};
use crate::client::space_names::SpaceSelector;

pub(super) async fn run(
    ctx: &Context,
    command: Option<DemoCommand>,
    name: Option<String>,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    // Without its token, the daemon would answer a workspace process as the user.
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "a workspace process that cleared its scope token cannot use shoal demo"
    );
    // Before a picker opens or anything is stopped or replaced.
    if let Some(name) = requested_name(command.as_ref(), name.as_deref()) {
        crate::demos::validate_name(name)?;
    }
    match command {
        None => open::open(ctx, name, workspace).await,
        Some(DemoCommand::Start {
            name,
            preset,
            scheme,
            open,
            ports,
            queue,
            workspace,
            command,
        }) => {
            let overrides = start::Overrides {
                preset,
                scheme,
                open,
                ports,
            };
            start::start(ctx, workspace, name, overrides, queue, command).await
        }
        Some(DemoCommand::Stop { name, workspace }) => stop::stop(ctx, workspace, name).await,
        Some(DemoCommand::Ls { workspace }) => list::list(ctx, workspace).await,
        Some(DemoCommand::Screen {
            name,
            lines,
            workspace,
        }) => screen::screen(ctx, workspace, &name, lines).await,
        Some(DemoCommand::Send {
            name,
            keys,
            workspace,
        }) => screen::send(ctx, workspace, &name, &keys).await,
    }
}

/// The demo name the command line gives, if it gives one.
fn requested_name<'a>(command: Option<&'a DemoCommand>, name: Option<&'a str>) -> Option<&'a str> {
    match command {
        None => name,
        Some(DemoCommand::Start { name, .. } | DemoCommand::Stop { name, .. }) => name.as_deref(),
        Some(DemoCommand::Screen { name, .. } | DemoCommand::Send { name, .. }) => Some(name),
        Some(DemoCommand::Ls { .. }) => None,
    }
}
