//! Tracked terminal launches, the agent's configured command with Shoal's
//! instructions, hooks and prompt, run through the execution wrapper.
use std::ffi::{OsStr, OsString};

use anyhow::Result;

use super::{Launch, LaunchOptions};
use crate::{
    agent::Agent,
    agents::{Integration, Wake, ensure_available},
    cli::{
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    config::{Effective, named_commands, templates},
    execution,
    model::Workspace,
};

/// The workspace a command such as `shoal codex` names, else the current
/// directory's or a picked one.
pub(super) async fn shortcut_workspace(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<Workspace> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    Ok(crate::client::workspaces::inspect(&ctx.paths, workspace)
        .await?
        .workspace)
}

/// Start `agent` in the workspace a shortcut names, as
/// [`super::launch_agent`] starts it.
pub(super) async fn shortcut(
    ctx: &Context,
    agent: Agent,
    workspace: Option<String>,
    subject: Option<templates::Subject>,
    args: Vec<OsString>,
    options: LaunchOptions,
) -> Result<i32> {
    let workspace = shortcut_workspace(ctx, workspace).await?;
    let opening = super::Opening::worker(subject);
    super::launch_agent(ctx, agent, workspace, opening, args, options).await
}

/// Run `launch` as a tracked agent in its workspace, in a session this
/// terminal attaches to where the terminal keeps agents in sessions.
pub(super) async fn tracked(
    ctx: &Context,
    integration: &dyn Integration,
    launch: Launch,
) -> Result<i32> {
    ensure_available(&launch.name, &launch.settings)?;
    let model_args = crate::agents::model_args(&launch.settings, &launch.name, &launch.model)?;
    if let Some(trust) = integration.trust_file() {
        let profile = &launch.settings.launch(&launch.name).environment;
        super::trust::tracked(trust, ctx, &launch.workspace, profile).await;
    }
    let command = command(ctx, &launch, model_args).await?;
    let Launch {
        name,
        workspace,
        settings,
        replaces,
        model,
        mode,
        ..
    } = launch;
    let agent = execution::TrackedAgent { name, model, mode };
    if let Some(code) =
        terminal::run_agent(ctx, &settings, &workspace, &agent, &replaces, &command).await?
    {
        return Ok(code);
    }
    terminal::label_current(&ctx.paths, crate::terminal::Owner::Workspace(&workspace.id)).await?;
    if replaces.is_empty() {
        return execution::run(&ctx.paths, workspace.id, command, Some(agent)).await;
    }
    execution::run_recovery(&ctx.paths, workspace.id, command, agent, replaces).await
}

/// The agent's configured command with the arguments that fork a session,
/// Shoal's instructions, `model_args`, its workspace arguments, the prompt
/// and the forwarded arguments.
async fn command(
    ctx: &Context,
    launch: &Launch,
    model_args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    let Launch {
        name,
        workspace,
        settings,
        instructions,
        prompt,
        args,
        fork,
        mode,
        ..
    } = launch;
    let (args, prompt) = input(
        settings,
        name,
        instructions.clone(),
        prompt.clone(),
        [
            model_args,
            crate::agents::workspace_args(&ctx.paths, settings, name, workspace, mode).await?,
        ]
        .concat(),
        args.clone(),
    );
    let args = fork.iter().cloned().chain(args).collect();
    named_commands::expand_with_fields(
        &ctx.paths,
        &settings.launchers,
        name,
        workspace,
        args,
        &[("{prompt}", OsStr::new(&prompt))],
    )
    .await
}

/// How updates reach a chat's agent with `integration`, which runs without
/// a wrapper to prompt it: pushed when a hook of its own wakes it.
pub(super) fn chat_delivery(integration: &dyn Integration) -> templates::Delivery {
    match integration.wakes() {
        Wake::Hook => templates::Delivery::Pushed,
        Wake::Prompt | Wake::Never => templates::Delivery::Watched,
    }
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
    let instruction_args = settings
        .launch(name)
        .arguments
        .instruction_args(&instructions);
    let (instruction_args, prompt) = separate_instructions(instruction_args, instructions, prompt);
    let takes_prompt = settings
        .launchers
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
/// agent without `instruction_args`, the arguments that would carry them,
/// gets them ahead of its prompt.
pub(super) fn separate_instructions(
    instruction_args: Option<Vec<OsString>>,
    instructions: String,
    prompt: Option<String>,
) -> (Vec<OsString>, String) {
    let prompt = prompt.unwrap_or_default();
    match instruction_args {
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
