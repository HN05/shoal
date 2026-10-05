//! CLI agent selection and launch adapters.
mod desktop;
mod happy;
mod native;
mod trust;

use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use anyhow::{Result, ensure};

use crate::{
    agent::{Agent, CodexMode},
    cli::{client, context::Context, ui},
    config::{Effective, named_commands::Commands},
    fsutil,
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
) -> Result<Option<Agent>> {
    let settings = client::settings(&ctx.paths, target).await?;
    match agent.or(settings.default_agent.clone()) {
        Some(agent) => Ok(Some(agent)),
        None => pick_default_agent(ctx, &settings),
    }
}

/// The picker standing in for a missing `default_agent`.
pub(super) fn pick_default_agent(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    ensure!(
        ctx.interactive(),
        "no agent selected; pass --agent or set default_agent in the repository or global config"
    );
    pick_agent(ctx, settings)
}

fn pick_agent(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let mut choices = Vec::new();
    for label in Agent::possible_values().into_iter().chain(
        settings
            .commands
            .keys()
            .filter(|name| matches!(name.parse(), Ok(Agent::Custom(_))))
            .cloned(),
    ) {
        let agent = label
            .parse()
            .map_err(|()| anyhow::anyhow!("unknown agent"))?;
        if installed(&agent, &settings.commands, &search_path) {
            choices.push((Some(agent), label));
        }
    }
    choices.push((None, "No agent".into()));
    let choices: Vec<_> = choices
        .iter()
        .map(|(agent, label)| (agent.clone(), label.as_str()))
        .collect();
    ui::pick_choice(ctx, "Agent> ", &choices)
}

/// Whether every executable the agent starts is available, so the picker
/// offers only agents that can launch.
fn installed(agent: &Agent, commands: &Commands, search_path: &OsStr) -> bool {
    let programs = match agent {
        Agent::Happy(agent) => vec!["happy", agent.as_str()],
        Agent::Codex | Agent::Claude | Agent::Custom(_) => commands
            .get(&String::from(agent.clone()))
            .and_then(|argv| argv.first())
            .map(String::as_str)
            .into_iter()
            .collect(),
    };
    !programs.is_empty()
        && programs
            .into_iter()
            .all(|program| program_available(program, search_path))
}

/// Programs with placeholders or workspace-relative paths resolve only at
/// launch, so they are assumed available.
fn program_available(program: &str, search_path: &OsStr) -> bool {
    let path = Path::new(program);
    if path.is_absolute() {
        fsutil::is_executable(path).unwrap_or(false)
    } else if program.contains('/') || program.contains('{') {
        true
    } else {
        fsutil::find_executable(OsStr::new(program), search_path).is_some()
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

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::*;
    use crate::{agent::BuiltinAgent, config::named_commands};

    #[test]
    fn agents_are_installed_only_when_every_program_they_start_is_on_path() {
        let bin = tempfile::tempdir().unwrap();
        for program in ["claude", "probe"] {
            let path = bin.path().join(program);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut commands = named_commands::defaults();
        for (name, program) in [
            ("probe", "probe".to_owned()),
            ("missing", "missing".to_owned()),
            ("absolute", bin.path().join("probe").display().to_string()),
            ("relative", "./scripts/agent".to_owned()),
        ] {
            commands.insert(name.into(), vec![program]);
        }
        let search_path = bin.path().as_os_str();
        let installed = |agent: Agent| installed(&agent, &commands, search_path);

        assert!(installed(Agent::Claude));
        assert!(!installed(Agent::Codex));
        // Happy needs both its own CLI and the agent it runs.
        assert!(!installed(Agent::Happy(BuiltinAgent::Claude)));
        fs::copy(bin.path().join("claude"), bin.path().join("happy")).unwrap();
        assert!(installed(Agent::Happy(BuiltinAgent::Claude)));
        assert!(!installed(Agent::Happy(BuiltinAgent::Codex)));

        assert!(installed(Agent::Custom("probe".into())));
        assert!(installed(Agent::Custom("absolute".into())));
        assert!(installed(Agent::Custom("relative".into())));
        assert!(!installed(Agent::Custom("missing".into())));
        assert!(!installed(Agent::Custom("undefined".into())));
    }
}
