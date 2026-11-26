use anyhow::Result;
use shoal_protocol::launches::Launch;

use crate::{
    agent::{Agent, ModelChoice},
    cli::{agents, context::Context},
    config::Effective,
};

/// Another machine checks its own.
pub(in crate::cli::commands::start::settle) fn check_agent(
    launch: &Launch,
    settings: &Effective,
) -> Result<()> {
    if launch.machine.is_none() {
        crate::agents::checked_agent(settings, launch.agent.clone(), &launch.model)?;
    }
    Ok(())
}

/// Another machine picks its own.
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

pub(in crate::cli::commands) fn check_model(
    settings: &Effective,
    agent: &Agent,
    model: &ModelChoice,
) -> Result<()> {
    let resolved = crate::agents::resolve_model(settings, agent, model.clone());
    crate::agents::check_model(settings, agent, &resolved)
}
