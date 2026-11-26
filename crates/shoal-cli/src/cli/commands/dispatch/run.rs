use std::ffi::OsString;

use anyhow::Result;

use crate::cli::{
    commands::{configured, exec},
    context::Context,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    name: Option<String>,
    workspace: Option<SpaceSelector>,
    now: bool,
    args: Vec<OsString>,
) -> Result<i32> {
    match name {
        Some(name) => configured::run(&ctx, &name, workspace, now, args).await,
        None if args.is_empty() => configured::list(&ctx, workspace).await,
        None => exec::exec(&ctx, workspace, now, args).await,
    }
}
