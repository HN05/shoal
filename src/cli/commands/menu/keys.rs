//! What each key of the picker does: the command a key and the row it
//! chose become.
use anyhow::{Context as _, Result, ensure};

use super::{Choice, View, execute::execute_command, linked::view_command, rows::Row};
use crate::cli::commands::approvals;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        Command,
        args::{
            ConfirmationArgs, SpaceTargets,
            changes::ChangesCommand,
            coordination::{CoordinationCommand, QuestionsArgs},
            create::{CreateCommand, StartArgs},
            items::{ItemArgs, ItemsCommand, ViewArgs},
            lifecycle::LifecycleCommand,
            repository::{RepoCommand, RepositoryCommand},
            service::{DaemonCommand, ServiceCommand},
            sessions::SessionsCommand,
            workspace::WorkspaceCommand,
        },
        context::Context,
        ui::Key,
    },
    model::PresentSpace,
};

/// What a key of the picker does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
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

impl Action {
    /// Every action, in the order headers list them: by how often each is used.
    pub(super) const ALL: [Self; 9] = [
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

    /// The key that does it.
    pub(super) fn key(self) -> Key {
        match self {
            Self::Enter => Key::Enter,
            Self::Delete => Key::Ctrl('d'),
            Self::Execute => Key::Ctrl('e'),
            Self::Add => Key::Ctrl('a'),
            Self::Status => Key::Ctrl('o'),
            Self::Stop => Key::Ctrl('s'),
            Self::Attach => Key::Ctrl('t'),
            Self::Diff => Key::Ctrl('f'),
            Self::View => Key::Ctrl('v'),
        }
    }

    /// The action `key` does.
    pub(super) fn of(key: Key) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.key() == key)
            .context("unknown picker action")
    }

    /// What it does, for the header; Enter's depends on the view.
    pub(super) fn label(self, view: View) -> &'static str {
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

    /// Workspace processes cannot add, delete, stop or attach.
    pub(super) fn allowed_scoped(self) -> bool {
        matches!(
            self,
            Self::Enter | Self::Execute | Self::Status | Self::Diff | Self::View
        )
    }
}

/// Add adds wherever it is pressed; Enter does what the row asks for, and
/// other keys act on the row's workspace.
pub(super) async fn command(ctx: &Context, action: Action, row: Row) -> Result<Choice> {
    if action == Action::Add {
        return Ok(Choice::Run(Box::new(add())));
    }
    if action == Action::Enter {
        return enter(ctx, row).await;
    }
    let space = row.space();
    ensure!(space.is_some(), "select a space for this action");
    if let Some(chat) = chat_name(ctx, space.as_ref()).await? {
        return Ok(Choice::Run(Box::new(
            chat_command(ctx, action, SpaceSelector::parse(&chat)).await?,
        )));
    }
    Ok(Choice::Run(Box::new(
        workspace_command(ctx, action, space).await?,
    )))
}

/// The name of the chat `space` names, if it is one, whose inbox rows only
/// some keys act on.
async fn chat_name(ctx: &Context, space: Option<&SpaceSelector>) -> Result<Option<String>> {
    let Some(space) = space else {
        return Ok(None);
    };
    Ok(
        match crate::client::workspaces::present_space(&ctx.paths, space).await? {
            Some(PresentSpace::Chat(chat)) => Some(chat.name.into()),
            _ => None,
        },
    )
}

/// What `action` does to the chat `chat`: only attaching, viewing its items
/// and removing it act on a chat.
async fn chat_command(ctx: &Context, action: Action, chat: SpaceSelector) -> Result<Command> {
    let chat = Some(chat);
    Ok(match action {
        Action::Delete => Command::Lifecycle(LifecycleCommand::Rm {
            space: chat.into_iter().collect(),
            confirmation: ConfirmationArgs::default(),
        }),
        Action::Attach => Command::Sessions(SessionsCommand::Attach {
            target: chat,
            session: None,
        }),
        Action::View => view_command(ctx, chat).await?,
        _ => anyhow::bail!("this action acts only on workspaces"),
    })
}

/// Answer a question, decide an approval as it was listed, attach to an
/// agent that needs input, show a failure, view ready work, open a demo,
/// show the daemon's status for failing cleanup, or enter a workspace.
async fn enter(ctx: &Context, row: Row) -> Result<Choice> {
    Ok(Choice::Run(Box::new(match row {
        Row::Question { number, .. } => Command::Coordination(CoordinationCommand::Questions {
            command: None,
            args: QuestionsArgs {
                number: Some(number),
                ..Default::default()
            },
        }),
        Row::Approval {
            number, created_at, ..
        } => {
            return Ok(Choice::Done(
                approvals::decide_listed(ctx, number, created_at).await?,
            ));
        }
        // A chat attaches by its name.
        Row::NeedsInput(space) => {
            let space = SpaceSelector::Id(space);
            Command::Sessions(SessionsCommand::Attach {
                target: Some(match chat_name(ctx, Some(&space)).await? {
                    Some(chat) => SpaceSelector::parse(&chat),
                    None => space,
                }),
                session: None,
            })
        }
        Row::Failed(workspace) => Command::Workspace(WorkspaceCommand::Status {
            space: Some(SpaceSelector::Id(workspace)),
            item: None,
        }),
        Row::Ready {
            space,
            url: Some(url),
        } => Command::Items(ItemsCommand::View(ViewArgs {
            items: ItemArgs {
                kind_or_url: Some(url),
                item: None,
                space: Some(SpaceSelector::Id(space)),
            },
            repo: Default::default(),
            no_comments: false,
            check: None,
            job: None,
            job_id: None,
            web: false,
        })),
        Row::Ready { space, url: None } => {
            match chat_name(ctx, Some(&SpaceSelector::id(&space))).await? {
                // A chat marked itself, so its agent says what is ready.
                Some(chat) => Command::Sessions(SessionsCommand::Attach {
                    target: Some(SpaceSelector::parse(&chat)),
                    session: None,
                }),
                None => Command::Workspace(WorkspaceCommand::Status {
                    space: Some(SpaceSelector::Id(space)),
                    item: None,
                }),
            }
        }
        Row::Demo { workspace, name } => Command::Sessions(SessionsCommand::Demo {
            command: None,
            name: Some(name),
            workspace: Some(SpaceSelector::Id(workspace)),
        }),
        Row::Untracked(path) => Command::Workspace(WorkspaceCommand::Adopt {
            path,
            repository: None,
            copy: false,
        }),
        Row::Cleanup => Command::Service(ServiceCommand::Daemon {
            command: DaemonCommand::Status,
        }),
        Row::SyncFailed(repository) => Command::Repository(RepositoryCommand::Repo {
            command: Some(RepoCommand::Sync {
                repository: Some(repository.into()),
            }),
        }),
        Row::Workspace(workspace) => Command::Workspace(WorkspaceCommand::Cd {
            workspace: Some(SpaceSelector::id(&workspace)),
        }),
        Row::Notification(name) => {
            ensure!(!name.is_empty(), "the notification names no workspace");
            Command::Workspace(WorkspaceCommand::Cd {
                workspace: Some(SpaceSelector::parse(&name)),
            })
        }
        Row::Add => add(),
    })))
}

/// “+ Add workspace” starts a worker in a new workspace.
fn add() -> Command {
    Command::Create(CreateCommand::Start(Box::new(StartArgs {
        mode: Some(crate::config::modes::ModeName::DEFAULT),
        ..Default::default()
    })))
}

/// What `action` does to `workspace`.
async fn workspace_command(
    ctx: &Context,
    action: Action,
    workspace: Option<SpaceSelector>,
) -> Result<Command> {
    Ok(match action {
        Action::Enter => Command::Workspace(WorkspaceCommand::Cd { workspace }),
        Action::Delete => Command::Lifecycle(LifecycleCommand::Rm {
            space: workspace.into_iter().collect(),
            confirmation: ConfirmationArgs::default(),
        }),
        Action::Status => Command::Workspace(WorkspaceCommand::Status {
            space: workspace,
            item: None,
        }),
        Action::Stop => Command::Lifecycle(LifecycleCommand::Stop {
            targets: SpaceTargets {
                space: workspace.into_iter().collect(),
                all: false,
            },
            yes: false,
            skip_children: false,
            interrupt: false,
            processes: false,
            leftovers: false,
        }),
        Action::Attach => Command::Sessions(SessionsCommand::Attach {
            target: workspace,
            session: None,
        }),
        Action::Diff => Command::Changes(ChangesCommand::Diff { workspace }),
        Action::View => view_command(ctx, workspace).await?,
        Action::Execute => execute_command(ctx, workspace).await?,
        Action::Add => add(),
    })
}
