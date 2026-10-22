//! The bare `shoal` picker, which `shoal work` also opens on a terminal: an
//! fzf list whose keys act on the selected row, binding only the keys that
//! mean something in the view shown. A selection plus key becomes an
//! ordinary [`Command`].
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        Command, ConfirmationArgs, WorkspaceScope, agents, client,
        context::Context,
        output::{Palette, Style},
        ui::{self, Entries, ViewPicker},
    },
    env,
    protocol::ConfigTarget,
};

const ADD_ENTRY: &str = "add-workspace";

/// A key the picker binds, named by what it does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Enter,
    Delete,
    Execute,
    Add,
    Inspect,
    Stop,
    Attach,
    Diff,
}

impl Key {
    const ALL: [Self; 8] = [
        Self::Enter,
        Self::Delete,
        Self::Execute,
        Self::Add,
        Self::Inspect,
        Self::Stop,
        Self::Attach,
        Self::Diff,
    ];

    /// Its fzf name.
    fn name(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Delete => "ctrl-d",
            Self::Execute => "ctrl-e",
            Self::Add => "ctrl-a",
            Self::Inspect => "ctrl-o",
            Self::Stop => "ctrl-s",
            Self::Attach => "ctrl-t",
            Self::Diff => "ctrl-f",
        }
    }

    /// What it does, for the header; Enter's depends on the view.
    fn label(self, view: View) -> &'static str {
        match self {
            Self::Enter => view.enter_label(),
            Self::Delete => "delete",
            Self::Execute => "execute",
            Self::Add => "add",
            Self::Inspect => "inspect",
            Self::Stop => "stop",
            Self::Attach => "attach",
            Self::Diff => "diff",
        }
    }

    /// The key fzf reports; a picker that prints nothing pressed Enter.
    fn parse(name: &str) -> Result<Self> {
        let name = if name.is_empty() { "enter" } else { name };
        Self::ALL
            .into_iter()
            .find(|key| key.name() == name)
            .context("unknown picker action")
    }

    /// Workspace processes cannot add, delete, stop or attach.
    fn allowed_scoped(self) -> bool {
        matches!(
            self,
            Self::Enter | Self::Execute | Self::Inspect | Self::Diff
        )
    }
}

/// What the picker lists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Work,
}

impl View {
    fn prompt(self) -> &'static str {
        match self {
            Self::Work => "work> ",
        }
    }

    /// The keys the view binds, in header order.
    fn keys(self) -> &'static [Key] {
        match self {
            Self::Work => &[
                Key::Enter,
                Key::Delete,
                Key::Execute,
                Key::Add,
                Key::Inspect,
                Key::Stop,
                Key::Attach,
                Key::Diff,
            ],
        }
    }

    fn enter_label(self) -> &'static str {
        match self {
            Self::Work => "cd",
        }
    }

    /// The keys a caller with this scope may use here.
    fn bound(self, scoped: bool) -> Vec<Key> {
        self.keys()
            .iter()
            .copied()
            .filter(|key| !scoped || key.allowed_scoped())
            .collect()
    }

    fn header(self, keys: &[Key]) -> String {
        keys.iter()
            .map(|key| format!("{}: {}", key.name(), key.label(self)))
            .collect::<Vec<_>>()
            .join("   ")
    }
}

/// What a row stands for, by the picker id it is chosen with.
enum Row {
    Workspace(String),
    Add,
}

impl Row {
    fn parse(id: String) -> Self {
        if id == ADD_ENTRY {
            Self::Add
        } else {
            Self::Workspace(id)
        }
    }
}

pub(super) async fn choose(ctx: &Context) -> Result<Command> {
    let scoped = env::is_scoped();
    let view = View::Work;
    let keys = view.bound(scoped);
    let names: Vec<_> = keys.iter().map(|key| key.name()).collect();
    let picked = ui::pick_in_views(
        ctx,
        rows(ctx, view, scoped).await?,
        ViewPicker {
            prompt: view.prompt(),
            header: &view.header(&keys),
            keys: &names,
            unbound: &[],
            next: None,
        },
    )?;
    command(ctx, Key::parse(&picked.action)?, Row::parse(picked.id)).await
}

async fn rows(ctx: &Context, view: View, scoped: bool) -> Result<Entries> {
    match view {
        View::Work => work_rows(ctx, scoped).await,
    }
}

/// Every workspace, then an entry that adds one unless workspace processes ask.
async fn work_rows(ctx: &Context, scoped: bool) -> Result<Entries> {
    let repos = ui::repository_choices(client::repositories(&ctx.paths).await?).await?;
    let workspaces = client::workspaces(&ctx.paths).await?;
    let palette = Palette::stderr(ctx.json);
    let stopped = ui::stopped_workspaces(&ctx.paths, &workspaces);
    let rows = ui::workspace_rows(&workspaces, &repos, &stopped, palette, ui::picker_width());
    let mut entries: Entries = workspaces.into_iter().map(|w| w.id).zip(rows).collect();
    if !scoped {
        entries.push((
            ADD_ENTRY.into(),
            format!("{} Add workspace", palette.paint(Style::Success, "+")),
        ));
    }
    Ok(entries)
}

/// Add adds wherever it is pressed, and Enter on the add entry adds; other
/// keys act on the selected row's workspace.
async fn command(ctx: &Context, key: Key, row: Row) -> Result<Command> {
    let workspace = match row {
        _ if key == Key::Add => return Ok(add()),
        Row::Add if key == Key::Enter => return Ok(add()),
        Row::Add => None,
        Row::Workspace(id) => Some(id),
    };
    ensure!(workspace.is_some(), "select a workspace for this action");
    workspace_command(ctx, key, workspace).await
}

fn add() -> Command {
    Command::Add {
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
    }
}

/// What `key` does to `workspace`.
async fn workspace_command(ctx: &Context, key: Key, workspace: Option<String>) -> Result<Command> {
    Ok(match key {
        Key::Enter => Command::Cd { workspace },
        Key::Delete => Command::Rm {
            workspace,
            confirmation: ConfirmationArgs::default(),
        },
        Key::Inspect => Command::Inspect { workspace },
        Key::Stop => Command::Stop {
            scope: WorkspaceScope {
                workspace,
                all: false,
            },
        },
        Key::Attach => Command::Attach {
            workspace,
            session: None,
        },
        Key::Diff => Command::Diff { workspace },
        Key::Execute => execute_command(ctx, workspace).await?,
        Key::Add => add(),
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
