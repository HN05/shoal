use std::ffi::OsString;

use anyhow::Result;

use crate::cli::{
    agents,
    commands::{configured, workspaces},
    context::Context,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    name: Option<String>,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    match name {
        Some(name) => agents::run_named(&ctx, &name, workspace, args).await,
        None if args.is_empty() => configured::list(&ctx, workspace).await,
        None => workspaces::exec(&ctx, workspace, args).await,
    }
}

pub(in crate::cli::commands) async fn custom(ctx: Context, words: Vec<OsString>) -> Result<i32> {
    match configured::invocation(words)? {
        Some((name, workspace, args)) => agents::run_named(&ctx, &name, workspace, args).await,
        None => Ok(0),
    }
}
