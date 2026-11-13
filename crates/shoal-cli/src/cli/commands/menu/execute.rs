//! Starting a workspace's agent, or running a desktop app or a shell
//! command in it, as chosen.
use anyhow::{Context as _, Result};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        Command, agents,
        args::{
            create::{CreateCommand, StartArgs},
            workspace::{DesktopApp, WorkspaceCommand},
        },
        context::Context,
        ui,
    },
    protocol::ConfigTarget,
};

#[derive(Clone)]
enum ExecuteChoice {
    /// An agent, started as the workspace's worker here, as
    /// `shoal start --in <workspace> --mode worker --agent <name> --here`.
    Agent(crate::agent::Agent),
    /// A desktop app, opened as `shoal open --app <app>`.
    App(DesktopApp),
    Shell,
}

/// Desktop apps the menu offers beside agents, each with its label.
const APPS: [(&str, DesktopApp); 3] = [
    ("claude app", DesktopApp::Claude),
    ("codex app", DesktopApp::Codex),
    ("t3", DesktopApp::T3),
];

/// What to run in `workspace`, as chosen.
pub(super) async fn execute_command(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<Command> {
    let target = workspace
        .clone()
        .context("select a workspace to execute in")?;
    let settings = crate::config::settings(&ctx.paths, ConfigTarget::Workspace(target)).await?;
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let agents = agents::names(&settings)
        .into_iter()
        .filter(|name| agents::installed(name, &settings.launchers, &search_path))
        .map(|agent| (ExecuteChoice::Agent(agent.clone()), agent.into()));
    let apps = APPS
        .into_iter()
        .filter(|(_, app)| agents::app(*app).available(&search_path))
        .map(|(label, app)| (ExecuteChoice::App(app), label.to_owned()));
    let choices: Vec<_> = agents
        .chain(apps)
        .chain([(ExecuteChoice::Shell, "custom shell command".into())])
        .collect();
    let choices: Vec<_> = choices
        .iter()
        .map(|(choice, label)| (choice.clone(), label.as_str()))
        .collect();
    let choice = ui::pick_choice(ctx, "Execute> ", &choices)?;
    Ok(match choice {
        ExecuteChoice::Agent(name) => Command::Create(CreateCommand::Start(Box::new(StartArgs {
            in_workspace: Some(workspace),
            mode: Some(crate::config::modes::ModeName::DEFAULT),
            agent: Some(name),
            here: true,
            ..Default::default()
        }))),
        ExecuteChoice::App(app) => Command::Workspace(WorkspaceCommand::Open {
            workspace,
            app: Some(app),
            args: vec![],
        }),
        ExecuteChoice::Shell => Command::Run {
            name: None,
            workspace,
            args: vec![
                "sh".into(),
                "-c".into(),
                ui::input(ctx, "Shell command")?.into(),
            ],
        },
    })
}
