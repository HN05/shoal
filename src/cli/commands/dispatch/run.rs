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
    let name = configured::command_name(&words)?;
    // An undefined name fails before its arguments are parsed, so that neither
    // help nor an argument error presents it as a command.
    let named = configured::named_workspace(&words);
    let definition = if agents::is_agent(&ctx, &name, named.as_deref()).await? {
        None
    } else {
        Some(configured::definition(&ctx, &name, named).await?)
    };
    let Some((workspace, args)) = configured::invocation(words)? else {
        return Ok(0);
    };
    match definition {
        // The definition holds the workspace the arguments name.
        Some(definition) => configured::run_defined(&ctx, &name, definition, args).await,
        None => agents::run_named(&ctx, &name, workspace, args).await,
    }
}
