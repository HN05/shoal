//! Tracked terminal launches: the agent's configured command with Shoal's
//! instructions, hooks and prompt, run through the execution wrapper.
use std::ffi::{OsStr, OsString};

use anyhow::Result;

use super::{Integration, Launch, ensure_available};
use crate::{
    cli::{commands::configured, context::Context, terminal},
    config::{Effective, templates},
    execution,
};

/// Run `launch` as a tracked agent in its workspace, in a session this
/// terminal attaches to where the terminal keeps agents in sessions.
pub(super) async fn tracked(
    ctx: &Context,
    integration: &dyn Integration,
    launch: Launch,
) -> Result<i32> {
    ensure_available(&launch.name, &launch.settings)?;
    if let Some(trust) = integration.trust_file() {
        trust.trust_workspace(ctx, &launch.workspace).await;
    }
    let command = command(ctx, integration, &launch).await?;
    let Launch {
        name,
        workspace,
        settings,
        replaces,
        ..
    } = launch;
    if let Some(code) =
        terminal::run_agent(ctx, &settings, &workspace, &name, &replaces, &command).await?
    {
        return Ok(code);
    }
    if replaces.is_empty() {
        return execution::run(&ctx.paths, workspace.id, command, Some(name)).await;
    }
    execution::run_recovery(&ctx.paths, workspace.id, command, name, replaces).await
}

/// The agent's configured command with Shoal's instructions, its hooks, the
/// prompt and the forwarded arguments.
async fn command(
    ctx: &Context,
    integration: &dyn Integration,
    launch: &Launch,
) -> Result<Vec<OsString>> {
    let Launch {
        name,
        workspace,
        settings,
        prompt,
        args,
        ..
    } = launch;
    let instructions = templates::instructions(
        settings.agent_template.as_deref(),
        workspace,
        integration.delivery(ctx, settings),
    );
    let (instruction_args, prompt) =
        separate_instructions(settings, name, instructions, prompt.clone());
    let mut args = args.clone();
    let argv = &settings.commands[name];
    if !prompt.is_empty() && !argv.iter().any(|arg| arg.contains("{prompt}")) {
        args.insert(0, prompt.clone().into());
    }
    let args = instruction_args
        .into_iter()
        .chain(integration.hook_args()?)
        .chain(args)
        .collect();
    configured::expand_with_fields(
        &ctx.paths,
        &settings.commands,
        name,
        workspace,
        args,
        &[("{prompt}", OsStr::new(&prompt))],
    )
    .await
}

/// The arguments carrying `instructions` and the prompt left to send: an
/// agent without an instructions template gets them ahead of its prompt.
fn separate_instructions(
    settings: &Effective,
    name: &str,
    instructions: String,
    prompt: Option<String>,
) -> (Vec<OsString>, String) {
    let prompt = prompt.unwrap_or_default();
    match settings.arguments(name).instruction_args(&instructions) {
        Some(instruction_args) => (instruction_args, prompt),
        None => (
            Vec::new(),
            [instructions, prompt]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
    }
}
