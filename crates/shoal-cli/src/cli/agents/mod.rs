//! The CLI's side of the agents Shoal launches: picking one, and the names
//! help and completion offer.

use anyhow::{Context as _, Result, ensure};

use crate::{
    agent::Agent,
    agents::{ADDED, Added, ensure_available},
    cli::{args::space::DesktopApp, context::Context, ui},
    config::Effective,
    tools::ensure_program,
};
use shoal_workers::launch::desktop::{self, App};

pub(in crate::cli) use crate::agents::{installed, names};

/// The agent a start runs under `settings`: the one `named`, else
/// `agent.default`, else the one a picker picks on a terminal. Without
/// either, a start off a terminal names none, which the daemon refuses.
pub(in crate::cli) fn start_agent(
    ctx: &Context,
    settings: &Effective,
    named: Option<Agent>,
) -> Result<Option<Agent>> {
    match named.or(settings.agent.default.clone()) {
        Some(agent) => {
            ensure_available(&agent, settings)?;
            Ok(Some(agent))
        }
        None if ctx.interactive() => pick_installed_agent(ctx, settings),
        None => Ok(None),
    }
}

/// The picker standing in for a missing `agent.default`: the agents whose
/// programs are installed.
fn pick_installed_agent(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let installed = |name: &Agent| installed(name, &settings.launchers, &search_path);
    pick_agent(ctx, settings, installed, None)
}

/// The picker standing in for a missing reviewer: the agents with a review
/// template whose review command is installed, or “No agent”, which runs
/// no review.
pub(in crate::cli) fn pick_reviewer(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    let available = |name: &Agent| {
        settings.launch(name).arguments.review.is_some()
            && ensure_reviewer_available(name, settings).is_ok()
    };
    pick_agent(ctx, settings, available, Some("No agent"))
}

fn pick_agent(
    ctx: &Context,
    settings: &Effective,
    available: impl Fn(&Agent) -> bool,
    none: Option<&'static str>,
) -> Result<Option<Agent>> {
    ensure!(ctx.interactive(), crate::agents::NO_AGENT);
    let agents = names(settings);
    let mut choices: Vec<_> = agents
        .iter()
        .filter(|agent| available(agent))
        .map(|agent| (Some(agent.clone()), agent.name()))
        .collect();
    // A picker cannot run without fzf, which leaves the agent unpicked.
    if choices.is_empty() || !ui::can_pick(ctx) {
        return Ok(None);
    }
    if let Some(none) = none {
        choices.push((None, none));
    }
    let picked: Option<Agent> = ui::pick_choice(ctx, "Agent> ", &choices)?;
    // The picker offers agents by their own programs; the launch may need
    // a session host too, refused before anything is created.
    if let Some(agent) = &picked {
        ensure_available(agent, settings)?;
    }
    Ok(picked)
}

/// Agent names Shoal knows without configuration, for help and completion.
pub(in crate::cli) fn builtin_names() -> Vec<String> {
    let mut names: Vec<String> = crate::ai::PROVIDERS
        .iter()
        .map(|provider| provider.name.to_owned())
        .collect();
    for Added { name, .. } in ADDED {
        if !names.iter().any(|known| known == name) {
            names.push((*name).to_owned());
        }
    }
    names
}

/// Refuse a reviewer without a review template or whose review command's
/// program is missing. A review runs without a session, so it needs no
/// terminal host.
pub(in crate::cli) fn ensure_reviewer_available(name: &Agent, settings: &Effective) -> Result<()> {
    settings.check_reviewer(name)?;
    let program = settings
        .launchers
        .get(name.name())
        .and_then(|argv| argv.first())
        .with_context(|| {
            format!("unknown agent {name:?}; set [agents.{name}] command in Shoal config")
        })?;
    ensure_program(name.name(), program)
}

pub(in crate::cli) fn app(name: DesktopApp) -> &'static App {
    match name {
        DesktopApp::Claude => &desktop::CLAUDE,
        DesktopApp::Codex => &desktop::CODEX_APP,
        DesktopApp::T3 => &desktop::T3,
    }
}

#[cfg(test)]
mod tests;
