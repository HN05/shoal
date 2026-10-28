//! Tracked terminal launches, the agent's configured command with Shoal's
//! instructions, hooks and prompt, run through the execution wrapper, and
//! untracked handoffs to a desktop app.
use std::ffi::{OsStr, OsString};

use anyhow::{Result, ensure};

use super::{
    AgentEnvironment, Integration, Launch, LaunchOptions, Wake, desktop, ensure_available,
    trust::TrustFile,
};
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

/// Hand `workspace` to `program`'s desktop app with `args`, untracked,
/// trusting it first in the app's `trust` file, when it has one.
pub(super) async fn open_app(
    ctx: &Context,
    workspace: &Workspace,
    program: &str,
    trust: Option<&TrustFile>,
    args: Vec<OsString>,
) -> Result<i32> {
    super::ensure_program(&format!("{program} app"), program)?;
    ensure!(workspace.path.is_dir(), "workspace directory is missing");
    if let Some(trust) = trust {
        // The app runs with this process's environment, not the workspace's.
        let environment = AgentEnvironment::inherited(&ctx.paths.home);
        trust.trust_workspace(ctx, workspace, &environment).await;
    }
    desktop::open(program, &workspace.path, args).await
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
    terminal::label_current(&ctx.paths, terminal::Owner::Workspace(&workspace.id), &name).await?;
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
        delivery(integration, settings),
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

/// How updates reach an agent with `integration` launched under `settings`:
/// pushed when it wakes for them.
pub(super) fn delivery(integration: &dyn Integration, settings: &Effective) -> templates::Delivery {
    match integration.wakes() {
        Wake::Hook => templates::Delivery::Pushed,
        Wake::Prompt if terminal::agents_can_be_prompted(settings) => templates::Delivery::Pushed,
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
