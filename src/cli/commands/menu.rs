//! The bare `shoal` picker, which `shoal inbox` and `shoal work` also open
//! on a terminal: an fzf list per view that Tab cycles, reloading its rows
//! from `shoal internal menu-rows`. Each view binds only the keys that mean
//! something there. A selection plus key becomes an ordinary [`Command`].
use std::{
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use clap::ValueEnum;

use super::{
    inbox::{Inbox, Item},
    listing::ListFilter,
};
use crate::{
    cli::{
        Command, agents,
        args::{
            ConfirmationArgs, WorkspaceTargets,
            agents::AgentsCommand,
            changes::ChangesCommand,
            coordination::{CoordinationCommand, QuestionsArgs},
            create::CreateCommand,
            items::{ItemArgs, ItemsCommand, ViewArgs},
            lifecycle::LifecycleCommand,
            sessions::SessionsCommand,
            workspace::{WorkCommand, WorkspaceCommand},
        },
        client,
        client::request,
        context::Context,
        internal::{self, Worker},
        output::{Palette, Style},
        ui::{self, Entries, KeyBindings, PickerView, ViewPicker, WidthLimit},
    },
    daemon::notifications::Notification,
    env,
    forge::items::link,
    model::LinkedItem,
    protocol::{ConfigTarget, NotificationMethod},
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
    Status,
    Stop,
    Attach,
    Diff,
    View,
}

impl Key {
    /// Every key, in the order headers list them: by how often each is used.
    const ALL: [Self; 9] = [
        Self::Enter,
        Self::Attach,
        Self::Diff,
        Self::View,
        Self::Add,
        Self::Execute,
        Self::Status,
        Self::Stop,
        Self::Delete,
    ];

    /// Its fzf name.
    fn name(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Delete => "ctrl-d",
            Self::Execute => "ctrl-e",
            Self::Add => "ctrl-a",
            Self::Status => "ctrl-o",
            Self::Stop => "ctrl-s",
            Self::Attach => "ctrl-t",
            Self::Diff => "ctrl-f",
            Self::View => "ctrl-v",
        }
    }

    /// What it does, for the header; Enter's depends on the view.
    fn label(self, view: View) -> &'static str {
        match self {
            Self::Enter => view.enter_label(),
            Self::Delete => "delete",
            Self::Execute => "execute",
            Self::Add => "add",
            Self::Status => "status",
            Self::Stop => "stop",
            Self::Attach => "attach",
            Self::Diff => "diff",
            Self::View => "view",
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
            Self::Enter | Self::Execute | Self::Status | Self::Diff | Self::View
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
    fn binds(self, key: Key) -> bool {
        match self {
            Self::Inbox => !matches!(key, Key::Execute | Key::Stop | Key::Delete),
            Self::Notifications => matches!(key, Key::Enter | Key::Status | Key::Add),
            Self::Work => true,
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

    /// The keys a caller with this scope may use here.
    fn bound(self, scoped: bool) -> Vec<Key> {
        Key::ALL
            .into_iter()
            .filter(|&key| self.binds(key) && (!scoped || key.allowed_scoped()))
            .collect()
    }

    /// The bound keys, with Tab after Enter when there is another view.
    /// Wrapped to the picker's width, as fzf cuts a line that is wider.
    fn header(self, keys: &[Key], switches: bool) -> String {
        let mut labels: Vec<_> = keys
            .iter()
            .map(|key| format!("{}: {}", key.name(), key.label(self)))
            .collect();
        if switches {
            labels.insert(1, "tab: next view".into());
        }
        ui::wrapped(&labels, "   ", ui::picker_width())
    }

    /// The view's presentation and actions for the shared picker.
    fn description(self, scoped: bool, switches: bool) -> PickerView<'static> {
        let keys = self.bound(scoped);
        PickerView {
            prompt: self.prompt(),
            header: self.header(&keys, switches),
            keys: keys.into_iter().map(Key::name).collect(),
        }
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
    /// A running demo, by its name.
    Demo {
        workspace: String,
        name: String,
    },
    /// A worktree no workspace owns, by its path.
    Untracked(PathBuf),
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
            Item::Demo { demo } => Self::Demo {
                workspace,
                name: demo.launch.name.clone(),
            },
            Item::Untracked { worktree } => Self::Untracked(worktree.path.clone()),
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
            Self::Demo { workspace, name } => format!("demo:{workspace}:{name}"),
            Self::Untracked(path) => format!("untracked:{}", encode_path(path)),
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
        if kind == "untracked" {
            return Ok(Self::Untracked(decode_path(rest)?));
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
            "demo" => Self::Demo {
                workspace,
                name: detail.to_owned(),
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
            | Self::Demo { workspace, .. }
            | Self::Workspace(workspace) => Some(workspace),
            Self::Notification(name) => (!name.is_empty()).then_some(name.as_str()),
            Self::Untracked(_) | Self::Add => None,
        }
    }
}

/// A path as hex digits of its bytes, since a picker id must survive the
/// tabs, newlines and surrounding spaces a path may hold.
fn encode_path(path: &Path) -> String {
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_path(hex: &str) -> Result<PathBuf> {
    let invalid = || anyhow::anyhow!("picker returned an invalid row");
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| {
            hex.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(invalid)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Open the picker on `start`, or bare on the inbox unless nothing needs
/// the user, searching for the current workspace's items when it has any.
pub(super) async fn choose(ctx: &Context, start: Option<View>) -> Result<Choice> {
    let scoped = env::inherits_scope(&ctx.paths.state);
    let views = View::available(scoped);
    let (view, entries) = first_view(ctx, start, views, scoped).await?;
    let query = match start {
        None if view == View::Inbox => current_items(ctx, &entries).await?,
        _ => None,
    };
    let next = (views.len() > 1)
        .then(|| internal::shell_command(&ctx.paths, Worker::MenuNext))
        .transpose()?;
    let picked = ui::pick_in_views(
        ctx,
        entries,
        ViewPicker {
            view: view.description(scoped, next.is_some()),
            keys: &Key::ALL.map(Key::name),
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

/// Print `view`'s rows for the picker to reload, refusing the views a
/// workspace process may not see, whatever variables it cleared.
pub(super) async fn print_rows(ctx: &Context, view: View) -> Result<i32> {
    let scoped = env::inherits_scope(&ctx.paths.state);
    ensure!(
        View::available(scoped).contains(&view),
        "workspace processes cannot see the inbox or notifications; ask the user"
    );
    for (id, label) in rows(ctx, view, scoped).await? {
        outln!("{}", ui::picker_line(&id, &label));
    }
    Ok(0)
}

/// Print the fzf actions that switch from the view fzf shows, named by its
/// prompt, to the next one.
pub(super) fn print_next(ctx: &Context) -> Result<i32> {
    let scoped = env::inherits_scope(&ctx.paths.state);
    let views = View::available(scoped);
    let next = *ui::next_picker_view(views, |view| view.prompt())?;
    let name = next.to_possible_value().context("views have names")?;
    let reload = internal::shell_command(
        &ctx.paths,
        Worker::MenuRows {
            view: name.get_name(),
        },
    )?;
    outln!(
        "{}",
        next.description(scoped, true)
            .transition(&Key::ALL.map(Key::name), &reload)
    );
    Ok(0)
}

async fn rows(ctx: &Context, view: View, scoped: bool) -> Result<Entries> {
    match view {
        View::Inbox => inbox_rows(ctx).await,
        View::Work => work_rows(ctx, scoped).await,
        View::Notifications => notification_rows(ctx).await,
    }
}

/// Recent notifications, newest first; showing the unread ones marks them
/// read, as `shoal alerts` does.
async fn notification_rows(ctx: &Context) -> Result<Entries> {
    let method = NotificationMethod::ListNotifications {
        unread_only: false,
        limit: NOTIFICATIONS_SHOWN,
    };
    let notifications = client::pages::<Notification>(&ctx.paths, method).await?;
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
    let now = crate::time::unix_seconds() as i64;
    let cells = notifications
        .iter()
        .map(|notification| {
            [
                (
                    crate::time::local_ago(notification.created_at, now),
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
    request::<()>(
        &ctx.paths,
        NotificationMethod::MarkNotificationsRead { ids: unread },
    )
    .await
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
/// agent that needs input, show a failure, view ready work, open a demo,
/// or enter a workspace.
async fn enter(ctx: &Context, row: Row) -> Result<Choice> {
    Ok(Choice::Run(Box::new(match row {
        Row::Question { id, .. } => Command::Coordination(CoordinationCommand::Questions {
            command: None,
            args: QuestionsArgs {
                id: Some(id),
                ..Default::default()
            },
        }),
        Row::Approval { id, created_at, .. } => {
            return Ok(Choice::Done(
                super::approvals::decide_listed(ctx, id, created_at).await?,
            ));
        }
        Row::NeedsInput(workspace) => Command::Sessions(SessionsCommand::Attach {
            target: Some(workspace),
            session: None,
        }),
        Row::Failed(workspace) => Command::Workspace(WorkspaceCommand::Status {
            workspace: Some(workspace),
            item: None,
        }),
        Row::Ready {
            workspace,
            url: Some(url),
        } => Command::Items(ItemsCommand::View(ViewArgs {
            items: ItemArgs {
                kind_or_url: Some(url),
                item: None,
                workspace: Some(workspace),
            },
            no_comments: false,
            check: None,
            job: None,
            job_id: None,
            web: false,
        })),
        Row::Ready {
            workspace,
            url: None,
        } => Command::Workspace(WorkspaceCommand::Status {
            workspace: Some(workspace),
            item: None,
        }),
        Row::Demo { workspace, name } => Command::Sessions(SessionsCommand::Demo {
            command: None,
            name: Some(name),
            workspace: Some(workspace),
        }),
        Row::Untracked(path) => Command::Workspace(WorkspaceCommand::Work {
            command: Some(WorkCommand::Adopt {
                path,
                repository: None,
                copy: false,
            }),
            chats: false,
            repository: None,
            status: Vec::new(),
            print: false,
        }),
        Row::Workspace(workspace) => Command::Workspace(WorkspaceCommand::Cd {
            workspace: Some(workspace),
        }),
        Row::Notification(name) => {
            ensure!(!name.is_empty(), "the notification names no workspace");
            Command::Workspace(WorkspaceCommand::Cd {
                workspace: Some(name),
            })
        }
        Row::Add => add(),
    })))
}

fn add() -> Command {
    Command::Create(CreateCommand::Add {
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
        after: vec![],
        args: vec![],
    })
}

/// What `key` does to `workspace`.
async fn workspace_command(ctx: &Context, key: Key, workspace: Option<String>) -> Result<Command> {
    Ok(match key {
        Key::Enter => Command::Workspace(WorkspaceCommand::Cd { workspace }),
        Key::Delete => Command::Lifecycle(LifecycleCommand::Rm {
            workspace: workspace.into_iter().collect(),
            confirmation: ConfirmationArgs::default(),
        }),
        Key::Status => Command::Workspace(WorkspaceCommand::Status {
            workspace,
            item: None,
        }),
        Key::Stop => Command::Lifecycle(LifecycleCommand::Stop {
            targets: WorkspaceTargets {
                workspace: workspace.into_iter().collect(),
                all: false,
            },
            yes: false,
            skip_children: false,
            interrupt: false,
        }),
        Key::Attach => Command::Sessions(SessionsCommand::Attach {
            target: workspace,
            session: None,
        }),
        Key::Diff => Command::Changes(ChangesCommand::Diff { workspace }),
        Key::View => view_command(ctx, workspace).await?,
        Key::Execute => execute_command(ctx, workspace).await?,
        Key::Add => add(),
    })
}

/// View one of the issues and PRs `workspace` links, or open it in the
/// browser, as chosen.
async fn view_command(ctx: &Context, workspace: Option<String>) -> Result<Command> {
    let workspace = workspace.context("select a workspace to view its issues and PRs")?;
    let items = linked_items(ctx, &workspace).await?;
    ensure!(!items.is_empty(), "the workspace links no issue or PR");
    let (url, web) = pick_linked(ctx, &items)?;
    Ok(Command::Items(ItemsCommand::View(ViewArgs {
        items: ItemArgs {
            kind_or_url: Some(url),
            item: None,
            workspace: Some(workspace),
        },
        no_comments: false,
        check: None,
        job: None,
        job_id: None,
        web,
    })))
}

/// The issues and PRs `workspace` links in its own repository, the only
/// ones `shoal view` reads for it.
async fn linked_items(ctx: &Context, workspace: &str) -> Result<Vec<LinkedItem>> {
    let record = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|record| record.id == workspace)
        .with_context(|| format!("unknown workspace: {workspace}"))?;
    let items = record.links.items();
    if items.is_empty() {
        return Ok(items);
    }
    let remote = crate::repository::remote_url_from_path(&record.path)
        .await?
        .context("item lookup needs an origin remote")?;
    Ok(items
        .into_iter()
        .filter(|item| item.in_repository(&remote))
        .collect())
}

/// The URL of one of `items` and whether to open it in the browser, which
/// workspace processes cannot.
fn pick_linked(ctx: &Context, items: &[LinkedItem]) -> Result<(String, bool)> {
    let entries = items
        .iter()
        .map(|item| item.url.clone())
        .zip(linked_labels(items))
        .collect();
    let bindings: &[_] = &[
        ("enter", "view", false),
        ("ctrl-o", "open in browser", true),
    ];
    let bindings = if env::inherits_scope(&ctx.paths.state) {
        &bindings[..1]
    } else {
        bindings
    };
    let picked = ui::pick_with_keys(ctx, "View> ", entries, KeyBindings(bindings))?;
    Ok((picked.id, picked.action))
}

/// One aligned row per item: its kind, number, title, which Shoal does not
/// record for every item, and URL.
fn linked_labels(items: &[LinkedItem]) -> Vec<String> {
    let cells = items
        .iter()
        .map(|item| {
            let kind = item.kind.title();
            let number = link::item(item.kind, &item.url)
                .map(|(_, number)| format!("#{number}"))
                .unwrap_or_default();
            [
                (kind.to_owned(), Some(Style::Heading)),
                (number, None),
                (item.title.clone().unwrap_or_default(), None),
                (item.url.clone(), Some(Style::Muted)),
            ]
        })
        .collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 2, width });
    ui::aligned_to(cells, Palette::picker(), limit)
}

#[derive(Clone)]
enum ExecuteChoice {
    /// An agent, run as `shoal run <name>`.
    Agent(String),
    /// A desktop app, opened by the command it builds for a workspace.
    App(OpenApp),
    Shell,
}

/// The command that opens a desktop app in a workspace.
type OpenApp = fn(Option<String>) -> Command;

/// Desktop handoffs the menu offers beside agents: each label, app and
/// command that opens the app.
const APPS: [(&str, &agents::App, OpenApp); 3] = [
    ("claude app", &agents::CLAUDE_APP, |workspace| {
        Command::Agents(AgentsCommand::Claude {
            workspace,
            cli: false,
            app: true,
            args: vec![],
        })
    }),
    ("codex app", &agents::CODEX_APP, |workspace| {
        Command::Agents(AgentsCommand::Codex {
            workspace,
            cli: false,
            app: true,
            args: vec![],
        })
    }),
    ("t3", &agents::T3, |workspace| {
        Command::Agents(AgentsCommand::T3 {
            workspace,
            args: vec![],
        })
    }),
];

async fn execute_command(ctx: &Context, workspace: Option<String>) -> Result<Command> {
    let target = workspace
        .clone()
        .context("select a workspace to execute in")?;
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(target)).await?;
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let agents = agents::names(&settings)
        .into_iter()
        .filter(|name| agents::installed(name, &settings.launchers, &search_path))
        .map(|name| (ExecuteChoice::Agent(name.clone()), name));
    let apps = APPS
        .into_iter()
        .filter(|(_, app, _)| app.available(&search_path))
        .map(|(label, _, command)| (ExecuteChoice::App(command), label.to_owned()));
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
        ExecuteChoice::App(command) => command(workspace),
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

#[cfg(test)]
mod tests;
