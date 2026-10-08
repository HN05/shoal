//! The bare `shoal` invocation: an fzf menu over workspaces that turns a
//! selection plus key binding into an ordinary [`Command`].
use anyhow::{Context as _, Result, ensure};

use crate::{
    agent::{Agent, BuiltinAgent, CodexMode},
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
    Diff,
}

const BINDINGS: &[(&str, &str, MenuAction)] = &[
    ("enter", "enter", MenuAction::Enter),
    ("ctrl-d", "delete", MenuAction::Delete),
    ("ctrl-e", "execute", MenuAction::Execute),
    ("ctrl-a", "add", MenuAction::Add),
    ("ctrl-o", "inspect", MenuAction::Inspect),
    ("ctrl-s", "stop", MenuAction::Stop),
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
    let rows = ui::workspace_rows(&workspaces, &repos, &stopped, false, palette);
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
            path: None,
            repository: None,
            repository_override: None,
            branch: None,
            existing: None,
            issue: None,
            base: None,
            git_profile: None,
            agent: None,
            args: vec![],
        },
        MenuAction::Enter => Command::Cd { workspace },
        MenuAction::Delete => Command::Rm {
            workspace,
            confirmation: ConfirmationArgs::default(),
            keep_branch: false,
            delete_branch: false,
        },
        MenuAction::Inspect => Command::Inspect { workspace },
        MenuAction::Stop => Command::Stop {
            scope: WorkspaceScope {
                workspace,
                all: false,
            },
        },
        MenuAction::Diff => Command::Diff { workspace },
        MenuAction::Execute => execute_command(ctx, workspace).await?,
    })
}

#[derive(Clone, Copy)]
enum ExecuteChoice {
    Claude,
    Codex(CodexMode),
    Happy(BuiltinAgent),
    T3,
    Shell,
}

async fn execute_command(ctx: &Context, workspace: Option<String>) -> Result<Command> {
    let target = workspace
        .clone()
        .context("select a workspace to execute in")?;
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(target)).await?;
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let installed = |choice: &ExecuteChoice| match choice {
        ExecuteChoice::Claude => {
            agents::installed(&Agent::Claude, &settings.commands, &search_path)
        }
        ExecuteChoice::Codex(CodexMode::Cli) => {
            agents::installed(&Agent::Codex, &settings.commands, &search_path)
        }
        ExecuteChoice::Codex(CodexMode::App) => agents::program_available("codex", &search_path),
        ExecuteChoice::Happy(agent) => {
            agents::installed(&Agent::Happy(*agent), &settings.commands, &search_path)
        }
        ExecuteChoice::T3 => agents::program_available("t3", &search_path),
        ExecuteChoice::Shell => true,
    };
    let choices: Vec<_> = [
        (ExecuteChoice::Claude, "claude"),
        (ExecuteChoice::Codex(CodexMode::Cli), "codex cli"),
        (ExecuteChoice::Codex(CodexMode::App), "codex app"),
        (ExecuteChoice::Happy(BuiltinAgent::Claude), "happy claude"),
        (ExecuteChoice::Happy(BuiltinAgent::Codex), "happy codex"),
        (ExecuteChoice::T3, "t3"),
        (ExecuteChoice::Shell, "custom shell command"),
    ]
    .into_iter()
    .filter(|(choice, _)| installed(choice))
    .collect();
    let choice = ui::pick_choice(ctx, "Execute> ", &choices)?;
    Ok(match choice {
        ExecuteChoice::Claude => Command::Claude {
            workspace,
            args: vec![],
        },
        ExecuteChoice::Codex(mode) => Command::Codex {
            workspace,
            cli: mode == CodexMode::Cli,
            app: mode == CodexMode::App,
            args: vec![],
        },
        ExecuteChoice::Happy(agent) => Command::Happy {
            agent,
            workspace,
            prompt: None,
            args: vec![],
        },
        ExecuteChoice::T3 => Command::T3 {
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
