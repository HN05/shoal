use std::ffi::OsString;

use anyhow::Result;

use crate::cli::{
    agents,
    commands::{configured, workspaces},
    context::Context,
};
use crate::client::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    name: Option<String>,
    workspace: Option<SpaceSelector>,
    args: Vec<OsString>,
) -> Result<i32> {
    match name {
        Some(name) => agents::run_named(&ctx, &name, workspace, args).await,
        None if args.is_empty() => configured::list(&ctx, workspace).await,
        None => workspaces::exec(&ctx, workspace, args).await,
    }
}
