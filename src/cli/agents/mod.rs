//! CLI agent selection and launch adapters.
mod desktop;
mod happy;
mod native;
mod trust;

use std::ffi::OsString;

use anyhow::{Result, bail};

use crate::{
    agent::{Agent, CodexMode},
    cli::{client, context::Context, ui},
    config::Effective,
    model::Workspace,
    protocol::ConfigTarget,
};

pub(super) use desktop::open_app;
pub(super) use happy::happy;
pub(super) use native::{claude, codex};

/// The requested agent, else the configured `default_agent`, else a picker.
pub(super) async fn default_agent(
    ctx: &Context,
    target: ConfigTarget,
    agent: Option<Agent>,
) -> Result<Agent> {
    let settings = client::settings(&ctx.paths, target).await?;
    select_default_agent(ctx, &settings, agent)
}

pub(super) fn select_default_agent(
    ctx: &Context,
    settings: &Effective,
    agent: Option<Agent>,
) -> Result<Agent> {
    match agent.or(settings.default_agent.clone()) {
        Some(agent) => Ok(agent),
        None if ctx.interactive() => ui::pick(
            ctx,
            "Agent> ",
            Agent::possible_values()
                .into_iter()
                .chain(
                    settings
                        .commands
                        .keys()
                        .filter(|name| matches!(name.parse(), Ok(Agent::Custom(_))))
                        .cloned(),
                )
                .map(|value| (value.clone(), value))
                .collect(),
        )?
        .parse()
        .map_err(|()| anyhow::anyhow!("unknown agent")),
        None => bail!(
            "no agent selected; pass --agent or set default_agent in the repository or global config"
        ),
    }
}

/// Start an agent in a ready workspace, giving it the prompt the way it accepts one.
pub(super) async fn launch_agent(
    ctx: &Context,
    agent: Agent,
    codex_mode: Option<CodexMode>,
    workspace: Workspace,
    prompt: Option<String>,
    mut args: Vec<OsString>,
) -> Result<i32> {
    if let Some(prompt) = &prompt
        && !matches!(agent, Agent::Happy(_) | Agent::Custom(_))
    {
        args.insert(0, prompt.into());
    }
    match agent {
        Agent::Codex => codex(ctx, codex_mode, Some(workspace.id), args).await,
        Agent::Claude => claude(ctx, Some(workspace.id), args).await,
        Agent::Happy(agent) => happy(ctx, agent, Some(workspace.id), prompt, args).await,
        Agent::Custom(name) => native::custom_agent(ctx, &name, workspace, prompt, args).await,
    }
}
