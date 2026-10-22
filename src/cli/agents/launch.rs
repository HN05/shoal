//! Tracked terminal launches: the agent's configured command with Shoal's
//! instructions, hooks and prompt, run through the execution wrapper.
use std::ffi::{OsStr, OsString};

use anyhow::Result;

use super::{Integration, Launch, ensure_installed};
use crate::{
    cli::{context::Context, terminal},
    config::{Config, named_commands, templates},
    execution,
};

/// Run `launch` as a tracked agent in its workspace, in a session this
/// terminal attaches to where the terminal keeps agents in sessions.
pub(super) async fn tracked(
    ctx: &Context,
    integration: &dyn Integration,
    launch: Launch,
) -> Result<i32> {
    let Launch {
        name,
        workspace,
        settings,
        prompt,
        mut args,
        tracked: _,
        replaces,
    } = launch;
    ensure_installed(&name, &settings.commands)?;
    if let Some(trust) = integration.trust_file() {
        trust.trust_workspace(ctx, &workspace).await;
    }
    let instructions = templates::instructions(
        settings.agent_template.as_deref(),
        &workspace,
        integration.delivery(ctx, &settings),
    );
    let (instruction_args, prompt) = separate_instructions(ctx, &name, instructions, prompt)?;
    let argv = &settings.commands[&name];
    if !prompt.is_empty() && !argv.iter().any(|arg| arg.contains("{prompt}")) {
        args.insert(0, prompt.clone().into());
    }
    let args = instruction_args
        .into_iter()
        .chain(integration.hook_args()?)
        .chain(args)
        .collect();
    let command = named_commands::expand_with_fields(
        &ctx.paths,
        &settings.commands,
        &name,
        &workspace,
        args,
        &[("{prompt}", OsStr::new(&prompt))],
    )
    .await?;
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

/// The arguments carrying `instructions` and the prompt left to send: an
/// agent without an instructions template gets them ahead of its prompt.
fn separate_instructions(
    ctx: &Context,
    name: &str,
    instructions: String,
    prompt: Option<String>,
) -> Result<(Vec<OsString>, String)> {
    let agents = Config::load(&ctx.paths)?.ai;
    let prompt = prompt.unwrap_or_default();
    Ok(
        match crate::ai::arguments(&agents, name).instruction_args(&instructions) {
            Some(instruction_args) => (instruction_args, prompt),
            None => (
                Vec::new(),
                [instructions, prompt]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
        },
    )
}
