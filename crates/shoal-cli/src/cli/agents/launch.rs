//! Tracked terminal launches, the agent's configured command with Shoal's
//! instructions, hooks and prompt, run through the execution wrapper.
use shoal_workers::hosting;
use std::ffi::{OsStr, OsString};

use anyhow::Result;

use super::Launch;
use crate::{
    agent::Agent,
    agents::{Integration, ensure_available},
    cli::context::Context,
    config::Effective,
};
use shoal_protocol::space_names::SpaceSelector;

/// Run `launch` as a tracked agent in its space, in a session this
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
        super::trust::space(trust, ctx, &launch.space, profile).await;
    }
    let command = command(ctx, &launch, model_args).await?;
    let Launch {
        name,
        space: records,
        settings,
        replaces,
        model,
        mode,
        narrowing,
        new_session,
        ..
    } = launch;
    let agent = shoal_protocol::executions::TrackedAgent {
        name,
        model,
        mode,
        narrowing,
        new_session,
    };
    let space = hosting::AgentSpace::of_home(records.home()?);
    if let Some(code) = hosting::run_agent(
        ctx.caller(),
        &settings,
        &space,
        &agent,
        (&replaces, false),
        &command,
    )
    .await?
    {
        return Ok(code);
    }
    hosting::label_current(&ctx.paths, space.owner()).await?;
    let selector = SpaceSelector::id(space.owner().id());
    if replaces.is_empty() {
        return shoal_workers::execution::run(&ctx.paths, selector, command, Some(agent)).await;
    }
    let records = (replaces, None);
    shoal_workers::execution::run_recovery(&ctx.paths, selector, command, agent, records).await
}

/// The agent's configured command with the arguments that fork a session,
/// Shoal's instructions, `model_args`, its space's arguments, the prompt
/// and the forwarded arguments.
async fn command(
    ctx: &Context,
    launch: &Launch,
    model_args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    let Launch {
        name,
        space,
        settings,
        instructions,
        prompt,
        args,
        fork,
        ..
    } = launch;
    let home = space.home()?;
    let space_args = shoal_workers::homes::args(&ctx.paths, home, settings, name).await?;
    let (args, prompt) = input(
        settings,
        name,
        instructions.clone(),
        prompt.clone(),
        [model_args, space_args].concat(),
        args.clone(),
    );
    let args = fork.iter().cloned().chain(args).collect();
    shoal_workers::homes::expand(
        &ctx.paths,
        home,
        &settings.launchers,
        name.name(),
        args,
        &[("{prompt}", OsStr::new(&prompt))],
    )
    .await
}

/// What agent `name`'s configured command receives: the arguments carrying
/// `instructions`, then `hooks` and `args`, and the value of `{prompt}`. The
/// prompt leads `args` when the command has no `{prompt}`.
fn input(
    settings: &Effective,
    name: &Agent,
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
        .get(name.name())
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
