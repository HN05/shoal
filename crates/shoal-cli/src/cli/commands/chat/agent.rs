//! The agent a chat starts: the one `--agent` or `agent.default` names,
//! else one picked among the installed agents, refused with a model its
//! launch cannot pass on.
use anyhow::{Result, ensure};

use crate::{
    agent::{Agent, ModelChoice},
    cli::{agents, context::Context, ui},
    config::Effective,
};

/// `--agent`, else `agent.default`, else a picker of the installed agents.
pub(in crate::cli::commands) fn chat_agent(
    ctx: &Context,
    settings: &Effective,
    agent: Option<Agent>,
) -> Result<Agent> {
    match crate::agents::named_agent(settings, agent)? {
        Some(agent) => Ok(agent),
        None => pick_agent(ctx, settings),
    }
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

fn pick_agent(ctx: &Context, settings: &Effective) -> Result<Agent> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let names: Vec<_> = agents::names(settings)
        .into_iter()
        .filter(|name| agents::installed(name, &settings.launchers, &search_path))
        .collect();
    ensure!(!names.is_empty(), "no agent is installed");
    ui::require_argument(ctx, "agent", "agent")?;
    let choices: Vec<_> = names
        .iter()
        .map(|agent| (agent.clone(), agent.name()))
        .collect();
    let agent: Agent = ui::pick_choice(ctx, "Agent> ", &choices)?;
    crate::agents::ensure_available(&agent, settings)?;
    Ok(agent)
}
