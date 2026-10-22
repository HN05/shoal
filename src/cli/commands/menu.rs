//! The bare `shoal` picker, which `shoal inbox` and `shoal work` also open
//! on a terminal: an fzf list per view that Tab cycles, reloading its rows
//! from `shoal internal menu-rows`. Each view binds only the keys that mean
//! something there. A selection plus key becomes an ordinary [`Command`].
use anyhow::{Context as _, Result, ensure};
use clap::ValueEnum;

use super::{
    inbox::{Inbox, Item},
    listing::ListFilter,
};
use crate::{
    cli::{
        Command, ConfirmationArgs, ItemArgs, WorkspaceScope, agents, client,
        client::request,
        context::Context,
        output::{Palette, Style},
        ui::{self, Entries, ViewPicker, WidthLimit},
    },
    daemon::notifications::Notification,
    env,
    protocol::{ConfigTarget, Method},
};

const ADD_ENTRY: &str = "add-workspace";

/// How many recent notifications the notifications view lists.
const NOTIFICATIONS_SHOWN: u32 = 50;

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

/// What the picker lists: what needs the user, the work going on, or what
/// the daemon saw.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(crate) enum View {
    Inbox,
    Work,
    Notifications,
}

impl View {
    fn prompt(self) -> &'static str {
        match self {
            Self::Inbox => "inbox> ",
            Self::Work => "work> ",
            Self::Notifications => "notifications> ",
        }
    }

    /// The keys the view binds, in header order.
    fn keys(self) -> &'static [Key] {
        match self {
            Self::Inbox => &[Key::Enter, Key::Attach, Key::Inspect, Key::Diff, Key::Add],
            Self::Notifications => &[Key::Enter, Key::Inspect, Key::Add],
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
            Self::Inbox => "act",
            Self::Work | Self::Notifications => "cd",
        }
    }

    /// The views a caller with this scope can switch between; workspace
    /// processes cannot decide what needs the user or read notifications.
    fn available(scoped: bool) -> &'static [Self] {
        if scoped {
            &[Self::Work]
        } else {
            &[Self::Inbox, Self::Work, Self::Notifications]
        }
    }

    /// The view after the one fzf shows with `prompt`, or the first.
    fn after(prompt: &str, views: &[Self]) -> Self {
        let shown = views.iter().position(|view| view.prompt() == prompt);
        views[shown.map_or(0, |shown| (shown + 1) % views.len())]
    }

    /// The fzf names of the keys a caller with this scope may not use here.
    fn unbound(self, scoped: bool) -> Vec<&'static str> {
        let bound = self.bound(scoped);
        Key::ALL
            .into_iter()
            .filter(|key| !bound.contains(key))
            .map(Key::name)
            .collect()
    }

    /// The keys a caller with this scope may use here.
    fn bound(self, scoped: bool) -> Vec<Key> {
        self.keys()
            .iter()
            .copied()
            .filter(|key| !scoped || key.allowed_scoped())
            .collect()
    }

    /// The bound keys, with Tab after Enter when there is another view.
    fn header(self, keys: &[Key], switches: bool) -> String {
        let mut labels: Vec<_> = keys
            .iter()
            .map(|key| format!("{}: {}", key.name(), key.label(self)))
            .collect();
        if switches {
            labels.insert(1, "tab: next view".into());
        }
        labels.join("   ")
    }

    /// The fzf actions that switch to this view, reloading its rows with
    /// `reload`: rebind its keys, unbind the others, and name it.
    fn switch_to(self, scoped: bool, reload: &str) -> String {
        let keys = self.bound(scoped);
        let unbound = self.unbound(scoped);
        let bound: Vec<_> = keys.iter().map(|key| key.name()).collect();
        let mut actions = Vec::new();
        if !unbound.is_empty() {
            actions.push(format!("unbind({})", unbound.join(",")));
        }
        actions.extend([
            format!("rebind({})", bound.join(",")),
            format!("change-prompt({})", self.prompt()),
            format!("change-header({})", self.header(&keys, true)),
            // Last, so the command may hold any character.
            format!("reload:{reload}"),
        ]);
        actions.join("+")
    }
}

/// What a row stands for, by the picker id it is chosen with: an inbox
/// item with its workspace's ID, a workspace, or the entry that adds one.
enum Row {
    Question {
        id: i64,
        workspace: String,
    },
    /// A request by its number and when it was made, since a later request
    /// may reuse the number.
    Approval {
        id: i64,
        created_at: u64,
        workspace: String,
    },
    NeedsInput(String),
    Failed(String),
    Ready {
        workspace: String,
        url: Option<String>,
    },
    Workspace(String),
    /// A notification, by the name its workspace had, if any.
    Notification(String),
    Add,
}

impl Row {
    fn of(item: &Item) -> Self {
        let workspace = item.workspace_id().to_owned();
        match item {
            Item::Question { question } => Self::Question {
                id: question.id,
                workspace,
            },
            Item::Approval { request } => Self::Approval {
                id: request.id,
                created_at: request.created_at,
                workspace,
            },
            Item::NeedsInput { .. } => Self::NeedsInput(workspace),
            Item::Failed { .. } => Self::Failed(workspace),
            Item::Ready { mark, .. } => Self::Ready {
                workspace,
                url: mark.url.clone(),
            },
        }
    }

    /// Its picker id: the kind and its fields, colon-separated, with a URL
    /// last since it holds colons itself; a workspace row is its ID.
    fn id(&self) -> String {
        match self {
            Self::Question { id, workspace } => format!("question:{workspace}:{id}"),
            Self::Approval {
                id,
                created_at,
                workspace,
            } => format!("approval:{workspace}:{id}:{created_at}"),
            Self::NeedsInput(workspace) => format!("needs-input:{workspace}"),
            Self::Failed(workspace) => format!("failed:{workspace}"),
            Self::Ready { workspace, url } => {
                format!("ready:{workspace}:{}", url.as_deref().unwrap_or_default())
            }
            Self::Workspace(workspace) => workspace.clone(),
            Self::Notification(name) => format!("notification:{name}"),
            Self::Add => ADD_ENTRY.into(),
        }
    }

    fn parse(id: String) -> Result<Self> {
        if id == ADD_ENTRY {
            return Ok(Self::Add);
        }
        let Some((kind, rest)) = id.split_once(':') else {
            return Ok(Self::Workspace(id));
        };
        if kind == "notification" {
            return Ok(Self::Notification(rest.to_owned()));
        }
        let (workspace, detail) = rest.split_once(':').unwrap_or((rest, ""));
        let workspace = workspace.to_owned();
        let number = || detail.parse().context("picker returned an invalid row");
        Ok(match kind {
            "question" => Self::Question {
                id: number()?,
                workspace,
            },
            "approval" => {
                let (id, created_at) = detail.split_once(':').unwrap_or((detail, ""));
                let invalid = || anyhow::anyhow!("picker returned an invalid row");
                Self::Approval {
                    id: id.parse().map_err(|_| invalid())?,
                    created_at: created_at.parse().map_err(|_| invalid())?,
                    workspace,
                }
            }
            "needs-input" => Self::NeedsInput(workspace),
            "failed" => Self::Failed(workspace),
            "ready" => Self::Ready {
                workspace,
                url: (!detail.is_empty()).then(|| detail.to_owned()),
            },
            _ => anyhow::bail!("picker returned an unknown row"),
        })
    }

    fn workspace(&self) -> Option<&str> {
        match self {
            Self::Question { workspace, .. }
            | Self::Approval { workspace, .. }
            | Self::NeedsInput(workspace)
            | Self::Failed(workspace)
            | Self::Ready { workspace, .. }
            | Self::Workspace(workspace) => Some(workspace),
            Self::Notification(name) => (!name.is_empty()).then_some(name.as_str()),
            Self::Add => None,
        }
    }
}

/// Open the picker on `start`, or bare on the inbox unless nothing needs
/// the user, searching for the current workspace's items when it has any.
pub(super) async fn choose(ctx: &Context, start: Option<View>) -> Result<Choice> {
    let scoped = env::is_scoped();
    let views = View::available(scoped);
    let (view, entries) = first_view(ctx, start, views, scoped).await?;
    let query = match start {
        None if view == View::Inbox => current_items(ctx, &entries).await?,
        _ => None,
    };
    let next = (views.len() > 1)
        .then(|| self_command(ctx, &["menu-next"]))
        .transpose()?;
    let picked = ui::pick_in_views(
        ctx,
        entries,
        ViewPicker {
            prompt: view.prompt(),
            header: &view.header(&view.bound(scoped), next.is_some()),
            keys: &Key::ALL.map(Key::name),
            unbound: &view.unbound(scoped),
            next,
            query,
        },
    )?;
    command(ctx, Key::parse(&picked.action)?, Row::parse(picked.id)?).await
}

/// An exact search for the current workspace's name when `entries` hold
/// items of it.
async fn current_items(ctx: &Context, entries: &Entries) -> Result<Option<String>> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    let Some(current) = ui::current_workspace(&workspaces)? else {
        return Ok(None);
    };
    let has_items = entries.iter().any(|(id, _)| {
        Row::parse(id.clone()).is_ok_and(|row| row.workspace() == Some(current.id.as_str()))
    });
    Ok(has_items.then(|| format!("'{}", current.name)))
}

/// The view to open and its rows: `start`, or the first available view
/// with rows.
async fn first_view(
    ctx: &Context,
    start: Option<View>,
    views: &[View],
    scoped: bool,
) -> Result<(View, Entries)> {
    if let Some(view) = start {
        return Ok((view, rows(ctx, view, scoped).await?));
    }
    for &view in &views[..views.len() - 1] {
        let entries = rows(ctx, view, scoped).await?;
        if !entries.is_empty() {
            return Ok((view, entries));
        }
    }
    let last = views[views.len() - 1];
    Ok((last, rows(ctx, last, scoped).await?))
}

/// Print `view`'s rows for the picker to reload.
pub(super) async fn print_rows(ctx: &Context, view: View) -> Result<i32> {
    for (id, label) in rows(ctx, view, env::is_scoped()).await? {
        println!("{}", ui::picker_line(&id, &label));
    }
    Ok(0)
}

/// Print the fzf actions that switch from the view fzf shows, named by its
/// prompt, to the next one.
pub(super) fn print_next(ctx: &Context) -> Result<i32> {
    let scoped = env::is_scoped();
    let views = View::available(scoped);
    let prompt = std::env::var("FZF_PROMPT").unwrap_or_default();
    let next = View::after(&prompt, views);
    let name = next.to_possible_value().context("views have names")?;
    let reload = self_command(ctx, &["menu-rows", name.get_name()])?;
    println!("{}", next.switch_to(scoped, &reload));
    Ok(0)
}

/// This binary's `shoal internal <args>` for the same state directory, as
/// shell text for fzf to run.
fn self_command(ctx: &Context, args: &[&str]) -> Result<String> {
    let exe = std::env::current_exe().context("locate the shoal executable")?;
    let mut words = vec![
        exe.to_string_lossy().into_owned(),
        "--state-dir".into(),
        ctx.paths.state.to_string_lossy().into_owned(),
        "internal".into(),
    ];
    words.extend(args.iter().map(|arg| (*arg).to_owned()));
    Ok(crate::shell::quote(&words))
}

async fn rows(ctx: &Context, view: View, scoped: bool) -> Result<Entries> {
    match view {
        View::Inbox => inbox_rows(ctx).await,
        View::Work => work_rows(ctx, scoped).await,
        View::Notifications => notification_rows(ctx).await,
    }
}

/// Recent notifications, newest first; showing the unread ones marks them
/// read, as `shoal notifications` does.
async fn notification_rows(ctx: &Context) -> Result<Entries> {
    let method = Method::ListNotifications {
        unread_only: false,
        limit: NOTIFICATIONS_SHOWN,
    };
    let notifications = request::<Vec<Notification>>(&ctx.paths, method).await?;
    let labels = notification_labels(&notifications);
    mark_read(ctx, &notifications).await?;
    Ok(notifications
        .iter()
        .map(|notification| {
            Row::Notification(notification.workspace.clone().unwrap_or_default()).id()
        })
        .zip(labels)
        .collect())
}

/// One aligned row per notification: when, its workspace, and the message,
/// dimmed once read and cut to fit the picker.
fn notification_labels(notifications: &[Notification]) -> Vec<String> {
    let cells = notifications
        .iter()
        .map(|notification| {
            [
                (
                    crate::time::local_minutes(notification.created_at),
                    Some(Style::Muted),
                ),
                (
                    notification.workspace.clone().unwrap_or_else(|| "-".into()),
                    Some(Style::Heading),
                ),
                (
                    notification.message.clone(),
                    notification.read.then_some(Style::Muted),
                ),
            ]
        })
        .collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 2, width });
    ui::aligned_to(cells, Palette::picker(), limit)
}

/// Mark the unread ones of `notifications` read.
async fn mark_read(ctx: &Context, notifications: &[Notification]) -> Result<()> {
    let unread: Vec<_> = notifications
        .iter()
        .filter(|notification| !notification.read)
        .map(|notification| notification.id)
        .collect();
    if unread.is_empty() {
        return Ok(());
    }
    request::<()>(&ctx.paths, Method::MarkNotificationsRead { ids: unread }).await
}

/// One row per item that needs the user.
async fn inbox_rows(ctx: &Context) -> Result<Entries> {
    let filter = ListFilter::resolve(ctx, None, Vec::new()).await?;
    let inbox = Inbox::request(ctx, &filter).await?;
    let repos = ui::repository_choices(client::repositories(&ctx.paths).await?).await?;
    let labels = inbox.rows(&repos, Palette::picker(), ui::picker_width());
    Ok(inbox
        .items
        .iter()
        .map(|item| Row::of(item).id())
        .zip(labels)
        .collect())
}

/// Every workspace, then an entry that adds one unless workspace processes ask.
async fn work_rows(ctx: &Context, scoped: bool) -> Result<Entries> {
    let repos = ui::repository_choices(client::repositories(&ctx.paths).await?).await?;
    let workspaces = client::workspaces(&ctx.paths).await?;
    let palette = Palette::picker();
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

/// What the picker chose: a command to run, or the exit code of an action
/// it finished itself.
pub(super) enum Choice {
    Run(Box<Command>),
    Done(i32),
}

/// Add adds wherever it is pressed; Enter does what the row asks for, and
/// other keys act on the row's workspace.
async fn command(ctx: &Context, key: Key, row: Row) -> Result<Choice> {
    if key == Key::Add {
        return Ok(Choice::Run(Box::new(add())));
    }
    if key == Key::Enter {
        return enter(ctx, row).await;
    }
    let workspace = row.workspace().map(str::to_owned);
    ensure!(workspace.is_some(), "select a workspace for this action");
    Ok(Choice::Run(Box::new(
        workspace_command(ctx, key, workspace).await?,
    )))
}

/// Answer a question, decide an approval as it was listed, attach to an
/// agent that needs input, inspect a failure, view ready work, or enter a
/// workspace.
async fn enter(ctx: &Context, row: Row) -> Result<Choice> {
    Ok(Choice::Run(Box::new(match row {
        Row::Question { id, .. } => Command::Answer {
            id: Some(id),
            answer: None,
        },
        Row::Approval { id, created_at, .. } => {
            return Ok(Choice::Done(
                super::approvals::decide_listed(ctx, id, created_at).await?,
            ));
        }
        Row::NeedsInput(workspace) => Command::Attach {
            workspace: Some(workspace),
            session: None,
        },
        Row::Failed(workspace) => Command::Inspect {
            workspace: Some(workspace),
        },
        Row::Ready {
            workspace,
            url: Some(url),
        } => Command::View {
            items: ItemArgs {
                kind_or_url: Some(url),
                item: None,
                workspace: Some(workspace),
            },
            no_comments: false,
            check: None,
        },
        Row::Ready {
            workspace,
            url: None,
        } => Command::Status {
            workspace: Some(workspace),
            item: None,
        },
        Row::Workspace(workspace) => Command::Cd {
            workspace: Some(workspace),
        },
        Row::Notification(name) => {
            ensure!(!name.is_empty(), "the notification names no workspace");
            Command::Cd {
                workspace: Some(name),
            }
        }
        Row::Add => add(),
    })))
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
