//! The bare `shoal` invocation: an fzf menu over workspaces that turns a
//! selection plus key binding into an ordinary [`Command`].
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        Command, ConfirmationArgs, WorkspaceScope, agents, client,
        context::Context,
        output::{Palette, Style},
        ui::{self, KeyBindings},
    },
    env,
    protocol::ConfigTarget,
};

const ADD_ENTRY: &str = "add-workspace";

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    Enter,
    Delete,
    Execute,
    Add,
    Inspect,
    Stop,
    Attach,
    Diff,
}

const BINDINGS: &[(&str, &str, MenuAction)] = &[
    ("enter", "enter", MenuAction::Enter),
    ("ctrl-d", "delete", MenuAction::Delete),
    ("ctrl-e", "execute", MenuAction::Execute),
    ("ctrl-a", "add", MenuAction::Add),
    ("ctrl-o", "inspect", MenuAction::Inspect),
    ("ctrl-s", "stop", MenuAction::Stop),
    ("ctrl-t", "attach", MenuAction::Attach),
    ("ctrl-f", "diff", MenuAction::Diff),
];

impl MenuAction {
    fn allowed_scoped(self) -> bool {
        matches!(
            self,
            Self::Enter | Self::Execute | Self::Inspect | Self::Diff
        )
    }
}

pub(super) async fn choose(ctx: &Context) -> Result<Command> {
    let repos = ui::repository_choices(client::repositories(&ctx.paths).await?).await?;
    let workspaces = client::workspaces(&ctx.paths).await?;
    let palette = Palette::stderr(ctx.json);
    let stopped = ui::stopped_workspaces(&ctx.paths, &workspaces);
    let rows = ui::workspace_rows(&workspaces, &repos, &stopped, palette);
    let mut entries: Vec<_> = workspaces.into_iter().map(|w| w.id).zip(rows).collect();
    let scoped = env::is_scoped();
    if !scoped {
        entries.push((
            ADD_ENTRY.into(),
            format!("{} Add workspace", palette.paint(Style::Success, "+")),
        ));
    }
    let bindings: Vec<_> = BINDINGS
        .iter()
        .copied()
        .filter(|(_, _, action)| !scoped || action.allowed_scoped())
        .collect();
    let picked = ui::pick_with_keys(ctx, "Shoal> ", entries, KeyBindings(&bindings))?;
    let action = if picked.action == MenuAction::Enter && picked.id == ADD_ENTRY {
        MenuAction::Add
    } else {
        picked.action
    };
    ensure!(
        action == MenuAction::Add || picked.id != ADD_ENTRY,
        "select a workspace for this action"
    );
    let workspace = Some(picked.id);
    Ok(match action {
        MenuAction::Add => Command::Add {
            here: false,
            focus: false,
            path: None,
            repository: None,
            repository_override: None,
            branch: None,
            existing: None,
            issue: None,
            base: None,
            git_profile: None,
            agent: None,
            now: false,
            args: vec![],
        },
        MenuAction::Enter => Command::Cd { workspace },
        MenuAction::Delete => Command::Rm {
            workspace,
            confirmation: ConfirmationArgs::default(),
        },
        MenuAction::Inspect => Command::Inspect { workspace },
        MenuAction::Stop => Command::Stop {
            scope: WorkspaceScope {
                workspace,
                all: false,
            },
        },
        MenuAction::Attach => Command::Attach {
            workspace,
            session: None,
        },
        MenuAction::Diff => Command::Diff { workspace },
        MenuAction::Execute => execute_command(ctx, workspace).await?,
    })
}

#[derive(Clone)]
enum ExecuteChoice {
    /// An agent, run as `shoal run <name>`.
    Agent(String),
    /// A desktop app that opens the workspace.
    App(&'static str),
    Shell,
}

/// Desktop handoffs the menu offers beside agents.
const APPS: [(&str, &str); 2] = [("codex app", "codex"), ("t3", "t3")];

async fn execute_command(ctx: &Context, workspace: Option<String>) -> Result<Command> {
    let target = workspace
        .clone()
        .context("select a workspace to execute in")?;
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(target)).await?;
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let agents = agents::names(&settings)
        .into_iter()
        .filter(|name| agents::installed(name, &settings.commands, &search_path))
        .map(|name| (ExecuteChoice::Agent(name.clone()), name));
    let apps = APPS
        .into_iter()
        .filter(|(_, program)| agents::program_available(program, &search_path))
        .map(|(label, program)| (ExecuteChoice::App(program), label.to_owned()));
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
        ExecuteChoice::Agent(name) => Command::Run {
            name: Some(name),
            workspace,
            args: vec![],
        },
        ExecuteChoice::App("codex") => Command::Codex {
            workspace,
            cli: false,
            app: true,
            args: vec![],
        },
        ExecuteChoice::App(_) => Command::T3 {
            workspace,
            args: vec![],
        },
        ExecuteChoice::Shell => Command::Exec {
            workspace,
            command: vec![
                "sh".into(),
                "-c".into(),
                ui::input(ctx, "Shell command")?.into(),
            ],
        },
    })
}
