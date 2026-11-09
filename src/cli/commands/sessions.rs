//! Persistent agent sessions run in terminal hosts; attaching hands this
//! terminal to one of a workspace's or a running chat's.
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, anyhow, bail, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        agents,
        context::Context,
        recent,
        terminal::{self, Recorded},
        ui::{self, Fallback, NoCurrentWorkspace},
    },
    model::{Chat, Inspection},
    protocol::{ConfigTarget, Protection},
    sessions::Origin,
};

pub(super) async fn attach(
    ctx: &Context,
    target: Option<SpaceSelector>,
    session: Option<String>,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "attach to sessions outside scoped executions"
    );
    ensure!(ctx.interactive(), "attach requires a terminal");
    let target = Target::resolve(ctx, target).await?;
    let sessions = (target.attachable(ctx, session.as_deref()).await?)
        .map_err(|protection| anyhow!("{}", stopped_agent(target.name(), &protection)))?;
    let session = match (session, sessions.as_slice()) {
        (Some(name), _) => sessions
            .iter()
            .find(|session| session.name == name)
            .with_context(|| format!("{} has no session {name}", target.name()))?,
        (None, []) => bail!("{}", target.without_sessions()),
        (None, [only]) => only,
        (None, several) => {
            let labels: Vec<_> = several
                .iter()
                .map(crate::terminal::Session::label)
                .collect();
            let choices: Vec<_> = several
                .iter()
                .zip(&labels)
                .map(|(session, label)| (session, label.as_str()))
                .collect();
            ui::require_argument(ctx, "session", "session")?;
            ui::pick_choice(ctx, "Attach session> ", &choices)?
        }
    };
    if terminal::attach(target.name(), session).await? {
        target.record_detach(ctx).await;
    }
    Ok(0)
}

/// The name of the terminal session `shoal attach` takes in which the
/// daemon recorded the agent of the workspace with the ID `id` running.
pub(super) async fn agent_terminal_session(ctx: &Context, id: &SpaceId) -> Result<Option<String>> {
    let target = Target::inspected(ctx, SpaceSelector::id(id)).await?;
    let sessions = target.attachable(ctx, None).await?.unwrap_or_default();
    Ok(sessions
        .into_iter()
        .find(|session| session.agent.is_some())
        .map(|session| session.name))
}

/// What `shoal attach` returns to.
enum Target {
    /// A workspace with the places its executions were recorded running in
    /// and those Shoal created for it.
    Workspace(Box<Inspection>),
    Chat(Box<Chat>),
}

impl Target {
    /// The running chat or workspace `name` selects, where `-` is the one
    /// whose session this terminal last left running; without one, the
    /// current workspace, else the current checkout's only running chat,
    /// else a picker of the workspaces and chats with sessions.
    async fn resolve(ctx: &Context, name: Option<SpaceSelector>) -> Result<Self> {
        let chats = crate::client::chats::list(&ctx.paths).await?;
        if name.as_ref().is_some_and(recent::is_previous) {
            let id = recent::previous_space(ctx, &chats).await?;
            return Self::space(ctx, chats, id).await;
        }
        if let Some(name) = name {
            return match (chats.into_iter())
                .find(|chat| name.selects(&chat.id, &chat.name, chat.number))
            {
                Some(chat) => Ok(Self::Chat(Box::new(chat))),
                None => Self::workspace(ctx, Some(name), Fallback::CurrentDirectory).await,
            };
        }
        match Self::workspace(ctx, None, Fallback::CurrentDirectoryOnly).await {
            Err(error) if error.is::<NoCurrentWorkspace>() => {}
            current => return current,
        }
        if let Some(chat) = checkout_chat(ctx, &chats).await? {
            return Ok(Self::Chat(Box::new(chat)));
        }
        Self::picked(ctx, chats).await
    }

    async fn workspace(
        ctx: &Context,
        name: Option<SpaceSelector>,
        fallback: Fallback,
    ) -> Result<Self> {
        let workspace = ui::choose_workspace(ctx, name, fallback).await?;
        Self::inspected(ctx, workspace).await
    }

    async fn inspected(ctx: &Context, workspace: SpaceSelector) -> Result<Self> {
        let inspection = crate::client::workspaces::inspect(&ctx.paths, workspace).await?;
        Ok(Self::Workspace(Box::new(inspection)))
    }

    /// A workspace or one of `chats`, each with an agent session, from a picker.
    async fn picked(ctx: &Context, chats: Vec<Chat>) -> Result<Self> {
        let attachable = terminal::session_owners(&chat_places(&chats));
        let chats: Vec<_> = chats
            .into_iter()
            .filter(|chat| attachable.contains(chat.id.as_str()))
            .collect();
        let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
        let id = ui::pick_space(ctx, workspaces, &chats, "workspace").await?;
        Self::space(ctx, chats, id).await
    }

    /// The one of `chats` with the ID `id`, else the workspace with it.
    async fn space(ctx: &Context, chats: Vec<Chat>, id: SpaceId) -> Result<Self> {
        if let Some(chat) = chats.into_iter().find(|chat| chat.id == id) {
            return Ok(Self::Chat(Box::new(chat)));
        }
        Self::inspected(ctx, SpaceSelector::Id(id)).await
    }

    /// Remember a workspace or chat once its session was shown and still
    /// runs, so `attach -` returns to the session last detached from rather
    /// than the one last attached, which another terminal may still show.
    async fn record_detach(&self, ctx: &Context) {
        match self {
            Self::Workspace(inspection) => {
                recent::record_attach(ctx, &SpaceSelector::id(&inspection.workspace.id)).await;
            }
            Self::Chat(chat) => recent::record_chat_attach(ctx, &SpaceSelector::id(&chat.id)).await,
        }
    }

    /// The target's sessions but those of agents Shoal stopped, or why not
    /// as [`terminal::attachable`] gives it.
    async fn attachable(
        &self,
        ctx: &Context,
        named: Option<&str>,
    ) -> Result<Result<Vec<crate::terminal::Session>, Protection>> {
        let settings = crate::client::workspaces::settings(&ctx.paths, self.config()).await?;
        let sessions = terminal::sessions(&settings, &self.places()).await?;
        Ok(terminal::attachable(sessions, named))
    }

    /// Whose settings choose the hosts to look in.
    fn config(&self) -> ConfigTarget {
        match self {
            Self::Workspace(inspection) => {
                ConfigTarget::Workspace(SpaceSelector::id(&inspection.workspace.id))
            }
            Self::Chat(chat) => ConfigTarget::chat(chat),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Workspace(inspection) => &inspection.workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }

    /// Where the daemon recorded the target running, with its agents.
    fn places(&self) -> Vec<Recorded<'_>> {
        match self {
            Self::Workspace(inspection) => {
                Recorded::workspace(&inspection.executions, &inspection.places)
            }
            Self::Chat(chat) => Recorded::chat(chat),
        }
    }

    /// Why there is nothing to attach to, and for a chat where it runs instead.
    fn without_sessions(&self) -> String {
        match self {
            Self::Workspace(inspection) => {
                format!("{} has no agent sessions", inspection.workspace.name)
            }
            Self::Chat(chat) => match agents::chat_place(&chat.agent) {
                Some(place) => {
                    format!("{} runs in {place}; it has no session to attach", chat.name)
                }
                None => format!(
                    "{} has no session to attach; it runs in the terminal that started it",
                    chat.name
                ),
            },
        }
    }
}

/// Why `name`'s agent, which `protection` stopped, cannot be attached to.
fn stopped_agent(name: &str, protection: &Protection) -> String {
    format!(
        "{name}'s agent is stopped ({}) and resumes once {}; `shoal queue start --attach {name}` resumes it now and attaches",
        protection.reason, protection.resumes_when
    )
}

/// Where the daemon recorded `chats` running, by chat ID.
pub(in crate::cli) fn chat_places(chats: &[Chat]) -> Vec<(&str, &Origin)> {
    chats
        .iter()
        .filter_map(|chat| Some((chat.id.as_str(), chat.origin.as_ref()?)))
        .collect()
}

/// The only running chat of the registered checkout the current directory
/// is in.
pub(in crate::cli::commands) async fn checkout_chat(
    ctx: &Context,
    chats: &[Chat],
) -> Result<Option<Chat>> {
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let Some(repo) = super::issues::current_checkout(&repos).await? else {
        return Ok(None);
    };
    let mut running = (chats.iter()).filter(|chat| chat.repository_id.as_ref() == Some(&repo.id));
    Ok(match (running.next(), running.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    })
}
