//! The bare `shoal` invocation: an fzf menu over workspaces that turns a
//! selection plus key binding into an ordinary [`Command`].
use anyhow::{Result, ensure};

use crate::{
    agent::{BuiltinAgent, CodexMode},
    cli::{
        Command, ConfirmationArgs, client,
        context::Context,
        ui::{self, KeyBindings},
    },
    env,
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
    let mut entries: Vec<_> = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .map(|w| {
            let repo = repos
                .iter()
                .find(|(id, _)| id == &w.repository_id)
                .map(|(_, name)| name.as_str())
                .unwrap_or("unknown repository");
            (
                w.id,
                format!("{}  {repo}  {}  {}", w.name, w.state, w.branch),
            )
        })
        .collect();
    let scoped = env::is_scoped();
    if !scoped {
        entries.push((ADD_ENTRY.into(), "+ Add workspace".into()));
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
            path: None,
            repository: None,
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
        MenuAction::Stop => Command::Stop { workspace },
        MenuAction::Diff => Command::Diff { workspace },
        MenuAction::Execute => execute_command(ctx, workspace)?,
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

fn execute_command(ctx: &Context, workspace: Option<String>) -> Result<Command> {
    let choice = ui::pick_choice(
        ctx,
        "Execute> ",
        &[
            (ExecuteChoice::Claude, "claude"),
            (ExecuteChoice::Codex(CodexMode::Cli), "codex cli"),
            (ExecuteChoice::Codex(CodexMode::App), "codex app"),
            (ExecuteChoice::Happy(BuiltinAgent::Claude), "happy claude"),
            (ExecuteChoice::Happy(BuiltinAgent::Codex), "happy codex"),
            (ExecuteChoice::T3, "t3"),
            (ExecuteChoice::Shell, "custom shell command"),
        ],
    )?;
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
