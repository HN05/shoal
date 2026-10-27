//! Tracked terminal launches: the agent's configured command with Shoal's
//! instructions, hooks and prompt, run through the execution wrapper.
use std::ffi::{OsStr, OsString};

use anyhow::Result;

use super::{Integration, Launch, LaunchOptions, ensure_available};
use crate::{
    agent::Agent,
    cli::{
        client,
        commands::configured,
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    config::{Effective, templates},
    execution,
    model::Workspace,
};

/// The workspace a shortcut such as `shoal codex` names, else the current
/// directory's or a picked one.
pub(super) async fn shortcut_workspace(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<Workspace> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    Ok(client::inspect(&ctx.paths, workspace).await?.workspace)
}

/// Start `agent` in the workspace a shortcut names, as
/// [`super::launch_agent`] starts it.
pub(super) async fn shortcut(
    ctx: &Context,
    agent: Agent,
    workspace: Option<String>,
    prompt: Option<String>,
    args: Vec<OsString>,
    options: LaunchOptions,
) -> Result<i32> {
    let workspace = shortcut_workspace(ctx, workspace).await?;
    super::launch_agent(ctx, agent, workspace, prompt, args, options).await
}

/// Run `launch` as a tracked agent in its workspace, in a session this
/// terminal attaches to where the terminal keeps agents in sessions.
pub(super) async fn tracked(
    ctx: &Context,
    integration: &dyn Integration,
    launch: Launch,
) -> Result<i32> {
    ensure_available(&launch.name, &launch.settings)?;
    if let Some(trust) = integration.trust_file() {
        let profile = &launch.settings.launch(&launch.name).environment;
        trust.trust_tracked(ctx, &launch.workspace, profile).await;
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

/// The agent's configured command with Shoal's instructions, its workspace
/// arguments, the prompt and the forwarded arguments.
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
        settings.worker_template.as_deref(),
        workspace,
        integration.delivery(settings),
    );
    let (args, prompt) = input(
        settings,
        name,
        instructions,
        prompt.clone(),
        super::workspace_args(&ctx.paths, settings, name, workspace).await?,
        args.clone(),
    );
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

/// What agent `name`'s configured command receives: the arguments carrying
/// `instructions`, then `hooks` and `args`, and the value of `{prompt}`. The
/// prompt leads `args` when the command has no `{prompt}`.
pub(in crate::cli) fn input(
    settings: &Effective,
    name: &str,
    instructions: String,
    prompt: Option<String>,
    hooks: Vec<OsString>,
    mut args: Vec<OsString>,
) -> (Vec<OsString>, String) {
    let (instruction_args, prompt) = separate_instructions(settings, name, instructions, prompt);
    let takes_prompt = settings
        .commands
        .get(name)
        .is_some_and(|argv| argv.iter().any(|arg| arg.contains("{prompt}")));
    if !prompt.is_empty() && !takes_prompt {
        args.insert(0, prompt.clone().into());
    }
    let args = instruction_args
        .into_iter()
        .chain(hooks)
        .chain(args)
        .collect();
    (args, prompt)
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
    match settings
        .launch(name)
        .arguments
        .instruction_args(&instructions)
    {
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
