//! The agent a start runs: the one it names, else `agent.default`, else
//! one a terminal picks, with the model asked of it.
use anyhow::Result;
use shoal_protocol::launches::Launch;

use crate::{
    agent::{Agent, ModelChoice},
    cli::{agents, context::Context},
    config::Effective,
};

/// Refuse the agent `launch` names, else `agent.default`, and the model
/// it asks of it, before anything is asked, when `settings` cannot start
/// them; another machine checks its own.
pub(in crate::cli::commands::start::settle) fn check_agent(
    launch: &Launch,
    settings: &Effective,
) -> Result<()> {
    if launch.machine.is_none() {
        crate::agents::checked_agent(settings, launch.agent.clone(), &launch.model)?;
    }
    Ok(())
}

/// Name the agent of `launch` under `settings`: the one it names, else
/// `agent.default`, else on a terminal a picked one; another machine
/// picks its own.
pub(in crate::cli::commands::start::settle) fn pick_agent(
    ctx: &Context,
    launch: &mut Launch,
    settings: &Effective,
) -> Result<()> {
    if launch.machine.is_none() {
        launch.agent = agents::start_agent(ctx, settings, launch.agent.take())?;
    }
    Ok(())
}

/// Refuse `model` for `agent` before anything else is asked for, when its
/// launch cannot pass the values on.
pub(in crate::cli::commands) fn check_model(
    settings: &Effective,
    agent: &Agent,
    model: &ModelChoice,
) -> Result<()> {
    let resolved = crate::agents::resolve_model(settings, agent, model.clone());
    crate::agents::check_model(settings, agent, &resolved)
}
