//! The checks that refuse an agent launch before anything starts: an agent
//! that is off or missing, a fork it cannot start from, or a model its
//! launch cannot pass on.
use anyhow::{Context as _, Result, ensure};

use super::{added, agent_integration, agent_off, detached, launch_proxy, model_args};
use crate::{
    agent::{Agent, ModelChoice},
    config::{Effective, named_commands::Commands},
    model::SpaceKind,
    protocol::operations::QueuedLaunch,
    tools::ensure_program,
};

/// Refuse an agent that cannot run under `settings` before anything starts.
pub fn ensure_available(name: &Agent, settings: &Effective) -> Result<()> {
    if let Some(reason) = agent_off(settings, name) {
        anyhow::bail!("cannot start {name}: {reason}");
    }
    launch_proxy(settings, name)?;
    ensure_installed(name, settings)
}

/// Refuse an agent whose executables are missing, the session host a launch
/// here under `settings` starts it in among them.
fn ensure_installed(name: &Agent, settings: &Effective) -> Result<()> {
    let launcher = crate::terminal::launcher_program(settings);
    let programs = programs(name, &settings.launchers, launcher).with_context(|| {
        format!("unknown agent {name:?}; set [agents.{name}] command in Shoal config")
    })?;
    for program in programs {
        ensure_program(name.name(), program)?;
    }
    Ok(())
}

/// The executables an agent starts; `None` when it has no launcher. A
/// terminal agent also starts `launcher`, its session host's program.
pub fn programs<'a>(
    name: &Agent,
    commands: &'a Commands,
    launcher: Option<&'static str>,
) -> Option<Vec<&'a str>> {
    if let Some(programs) = added(name).and_then(|added| added.integration.programs()) {
        return Some(programs);
    }
    commands
        .get(name.name())
        .and_then(|argv| argv.first())
        .map(|program| {
            let mut programs = vec![program.as_str()];
            programs.extend(launcher);
            programs
        })
}

/// Refuse agent `name` when it cannot start from a fork of a session: it
/// has no arguments for one, or an app runs it apart from any terminal.
pub fn ensure_forks(settings: &Effective, name: &Agent) -> Result<()> {
    if let Some(app) = agent_integration(settings, name).detached() {
        anyhow::bail!(
            "cannot fork a session of {name}: {} cannot start one from a fork",
            app.name()
        );
    }
    ensure!(
        settings.launch(name).arguments.fork.is_some(),
        "cannot fork a session of {name}: configure [agents.{name}] fork"
    );
    Ok(())
}

/// The model and effort a launch of agent `name` runs with, as
/// [`Effective::model`] resolves them, with what it leaves to the tool taken
/// from the configuration of the agent an app it runs in starts.
pub fn resolve_model(settings: &Effective, name: &Agent, requested: ModelChoice) -> ModelChoice {
    let model = settings.model(name, requested);
    let started = agent_integration(settings, name)
        .detached()
        .and_then(|app| settings.tools.get(app.agent().name()));
    match started {
        Some(tool) => model.or(tool.model.clone()),
        None => model,
    }
}

/// Refuse agent `name` with `model` before anything starts when its launch
/// cannot pass the values on: through its own templates, or those of the
/// agent an app it runs in starts.
pub fn check_model(settings: &Effective, name: &Agent, model: &ModelChoice) -> Result<()> {
    match agent_integration(settings, name).detached() {
        Some(app) => detached::model_args(app, settings, name, model).map(drop),
        None => model_args(settings, name, model).map(drop),
    }
}

/// Refuse `launch` when it cannot run under `settings`: an agent that is
/// off or missing, a fork it cannot start from, or a model its launch
/// cannot pass on.
pub fn check_queued_launch(launch: &QueuedLaunch, settings: &Effective) -> Result<()> {
    let mode = (launch.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT);
    crate::config::modes::resolve(&settings.modes, &mode, SpaceKind::Workspace, true)?;
    let agent = &launch.agent;
    ensure_available(agent, settings)?;
    if launch.fork.is_some() {
        ensure_forks(settings, agent)?;
    }
    let model = resolve_model(settings, agent, launch.model.clone());
    check_model(settings, agent, &model)
}

#[cfg(test)]
mod tests;
