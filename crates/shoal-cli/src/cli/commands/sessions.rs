//! Persistent agent sessions run in terminal hosts; attaching hands this
//! terminal to one of a workspace's or a chat's, first waking at once an
//! agent that sleeps or is stopped there.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, bail, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        agents,
        args::SpaceTargets,
        commands::queued_agents,
        context::Context,
        recent,
        terminal::{self, Recorded},
        ui::{self, Fallback, NoCurrentWorkspace},
    },
    model::{Chat, Inspection},
    protocol::{ConfigTarget, Protection},
    sessions::Origin,
    state::{QueueKind, QueueState},
    terminal::Session,
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
    ensure!(ctx.on_terminal(), "attach requires a terminal");
    let target = Target::resolve(ctx, target).await?;
    let (target, session) = target.awake_session(ctx, session).await?;
    if terminal::attach(&target.number().to_string(), &session).await? {
        target.record_detach(ctx).await;
    }
    Ok(0)
}

/// The one of `target`'s `sessions` that `named` names, its only one, or one
/// picked among them.
fn chosen_session<'a>(
    ctx: &Context,
    target: &Target,
    named: Option<String>,
    sessions: &'a [Session],
) -> Result<&'a Session> {
    Ok(match (named, sessions) {
        (Some(name), _) => sessions
            .iter()
            .find(|session| session.name == name)
            .with_context(|| format!("{} has no session {name}", target.name()))?,
        (None, []) => bail!("{}", target.without_sessions()),
        (None, [only]) => only,
        (None, several) => {
            let labels: Vec<_> = several.iter().map(Session::label).collect();
            let choices: Vec<_> = several
                .iter()
                .zip(&labels)
                .map(|(session, label)| (session, label.as_str()))
                .collect();
            ui::require_argument(ctx, "session", "session")?;
            ui::pick_choice(ctx, "Attach session> ", &choices)?
        }
    })
}

/// The name of the terminal session `shoal attach` takes in which the
/// daemon recorded the agent of the workspace with the ID `id` running,
/// only `execution`'s when given.
pub(super) async fn agent_terminal_session(
    ctx: &Context,
    id: &SpaceId,
    execution: Option<&ExecutionId>,
) -> Result<Option<String>> {
    let target = Target::by_id(ctx, id.clone()).await?;
    let session = agent_session(target.sessions(ctx).await?, execution);
    Ok(session.map(|session| session.name))
}

/// The one of `sessions` where an agent runs that Shoal has not stopped,
/// only `execution`'s when given, since a host may give the places of
/// several agents one name.
fn agent_session(sessions: Vec<Session>, execution: Option<&ExecutionId>) -> Option<Session> {
    sessions.into_iter().find(|session| {
        session.agent.is_some()
            && session.stopped.is_none()
            && execution.is_none_or(|execution| session.execution.as_ref() == Some(execution))
    })
}

/// What `shoal attach` returns to.
enum Target {
    /// A workspace with the places its executions were recorded running in
    /// and those Shoal created for it.
    Workspace(Box<Inspection>),
    Chat(Box<Chat>),
}

impl Target {
    /// The chat or workspace `name` selects, where `-` is the one whose
    /// session this terminal last left running; without one, the current
    /// workspace, else the current checkout's only running chat, else a
    /// picker of the workspaces and the chats with sessions or an agent that
    /// sleeps or is stopped.
    async fn resolve(ctx: &Context, name: Option<SpaceSelector>) -> Result<Self> {
        let chats = crate::client::chats::recorded(&ctx.paths).await?;
        let running: Vec<Chat> = (chats.iter())
            .filter(|chat| chat.running)
            .cloned()
            .collect();
        if name.as_ref().is_some_and(recent::is_previous) {
            let id = recent::previous_space(ctx, &running).await?;
            return Self::space(ctx, running, id).await;
        }
        if let Some(name) = name {
            return match (chats.into_iter()).find(|chat| name.selects(&chat.id, chat.number)) {
                Some(chat) => Ok(Self::Chat(Box::new(chat))),
                None => Self::workspace(ctx, Some(name), Fallback::CurrentDirectory).await,
            };
        }
        match Self::workspace(ctx, None, Fallback::CurrentDirectoryOnly).await {
            Err(error) if error.is::<NoCurrentWorkspace>() => {}
            current => return current,
        }
        if let Some(chat) = checkout_chat(ctx, &running).await? {
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

    /// A workspace or one of `chats` with an agent session, or whose agent
    /// sleeps or is stopped, which the picker's status shows, from a picker.
    async fn picked(ctx: &Context, chats: Vec<Chat>) -> Result<Self> {
        let attachable = terminal::session_owners(&chat_places(&chats));
        let chats: Vec<_> = chats
            .into_iter()
            .filter(|chat| attachable.contains(&chat.id) || !chat.running || chat.asleep.is_some())
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

    /// The chat or workspace with the ID `id`, as it is now.
    async fn by_id(ctx: &Context, id: SpaceId) -> Result<Self> {
        let chats = crate::client::chats::recorded(&ctx.paths).await?;
        Self::space(ctx, chats, id).await
    }

    fn id(&self) -> &SpaceId {
        match self {
            Self::Workspace(inspection) => &inspection.workspace.id,
            Self::Chat(chat) => &chat.id,
        }
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

    /// The target's open sessions, each where an agent Shoal stopped waits
    /// marked with why.
    async fn sessions(&self, ctx: &Context) -> Result<Vec<Session>> {
        let settings = crate::config::settings(&ctx.paths, self.config()).await?;
        terminal::sessions(&settings, &self.places()).await
    }

    /// The session `named` names, the target's only one or one picked,
    /// taken once the agent there runs. With no session named and none
    /// showing an agent, an agent that sleeps wakes first, and a stopped one
    /// when no session is left to attach to; a session where Shoal stopped
    /// the agent wakes it.
    async fn awake_session(self, ctx: &Context, named: Option<String>) -> Result<(Self, Session)> {
        let sessions = self.sessions(ctx).await?;
        let shows_agent = sessions.iter().any(|session| session.agent.is_some());
        if named.is_none() && !shows_agent && self.wake(ctx, sessions.is_empty()).await? {
            return self.awaited(ctx, None).await;
        }
        let chosen = chosen_session(ctx, &self, named, &sessions)?.clone();
        let (Some(execution), Some(protection)) = (&chosen.execution, &chosen.stopped) else {
            return Ok((self, chosen));
        };
        if !super::queue::resume_stopped(ctx, execution).await? {
            bail!("{}", stopped_agent(&self, protection));
        }
        self.awaited(ctx, Some(execution)).await
    }

    /// Wake the target's agent at once, whatever the agent limits and the
    /// queue's rank: start the restore that waits for it, as for an agent
    /// that sleeps, or when `stopped` too restore its stopped work as
    /// `shoal restore --now` does. Whether it woke one.
    async fn wake(&self, ctx: &Context, stopped: bool) -> Result<bool> {
        let entries = queued_agents::entries(ctx).await?;
        let restore = (entries.into_iter())
            .find(|entry| entry.kind == QueueKind::Restore && entry.space() == Some(self.id()));
        if let Some(entry) = restore {
            if entry.state == QueueState::Queued {
                crate::client::queue::start(&ctx.paths, entry.id.to_string()).await?;
            }
            return Ok(true);
        }
        let space = SpaceSelector::id(self.id());
        if !stopped
            || crate::client::saved::read(&ctx.paths, space.clone())
                .await?
                .is_empty()
        {
            return Ok(false);
        }
        let targets = SpaceTargets {
            space: vec![space],
            all: false,
        };
        let now = super::restore::agents::Options {
            execution: None,
            forget: false,
            no_agents: false,
            now: true,
            focus: false,
            change: None,
        };
        super::restore::run(ctx, targets, None, now).await?;
        Ok(true)
    }

    /// The target, read again, with the session its agent, only
    /// `execution` when given, runs in once it does.
    async fn awaited(
        self,
        ctx: &Context,
        execution: Option<&ExecutionId>,
    ) -> Result<(Self, Session)> {
        let deadline = tokio::time::Instant::now() + super::queue::WAIT;
        let id = self.id().clone();
        loop {
            let target = Self::by_id(ctx, id.clone()).await?;
            if let Some(session) = agent_session(target.sessions(ctx).await?, execution) {
                return Ok((target, session));
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "{} has no agent session after {} seconds; shoal attach {} attaches once it has one",
                target.name(),
                super::queue::WAIT.as_secs(),
                target.number()
            );
            tokio::time::sleep(super::queue::POLL).await;
        }
    }

    /// Whose settings choose the hosts to look in.
    fn config(&self) -> ConfigTarget {
        match self {
            Self::Workspace(inspection) => {
                ConfigTarget::Space(SpaceSelector::id(&inspection.workspace.id))
            }
            Self::Chat(chat) => ConfigTarget::chat(chat),
        }
    }

    fn name(&self) -> &crate::client::space_names::SpaceName {
        match self {
            Self::Workspace(inspection) => &inspection.workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }

    fn number(&self) -> crate::client::space_ids::SpaceNumber {
        match self {
            Self::Workspace(inspection) => inspection.workspace.number,
            Self::Chat(chat) => chat.number,
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

/// Why `target`'s agent, which `protection` stopped, cannot be attached to
/// while no restore of it waits to start.
fn stopped_agent(target: &Target, protection: &Protection) -> String {
    format!(
        "{}'s agent is stopped ({}) and resumes once {}",
        target.name(),
        protection.reason,
        protection.resumes_when,
    )
}

/// Where the daemon recorded `chats` running, by chat ID.
pub(in crate::cli) fn chat_places(chats: &[Chat]) -> Vec<(&SpaceId, &Origin)> {
    chats
        .iter()
        .filter_map(|chat| Some((&chat.id, chat.origin.as_ref()?)))
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

#[cfg(test)]
mod tests;
