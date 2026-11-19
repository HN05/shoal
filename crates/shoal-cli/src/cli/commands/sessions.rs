//! Persistent agent sessions run in terminal hosts; attaching hands this
//! terminal to one of a workspace's or a chat's, first waking at once an
//! agent that sleeps or is stopped there. A user on another machine
//! attaches through a terminal the space's machine makes for them.
mod owner;
mod viewer;

pub(in crate::cli::commands) use owner::attach_for_viewer;

use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::status::Inspection;

use crate::{
    cli::{
        agents,
        args::SpaceTargets,
        context::Context,
        recent,
        terminal::{self, Recorded},
        ui::{self, Fallback, NoCurrentWorkspace},
    },
    client::connection::CommandPaths,
    model::{Chat, Protection, Spaces},
    protocol::ConfigTarget,
    sessions::Origin,
    terminal::Session,
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn attach(
    ctx: &Context,
    target: Option<SpaceSelector>,
    session: Option<String>,
) -> Result<i32> {
    ensure!(
        !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "attach to sessions outside scoped executions"
    );
    ensure!(ctx.on_terminal(), "attach requires a terminal");
    if let Some(selector) = &target
        && let Some((remote, view)) = super::machines::space_elsewhere(ctx, selector).await?
    {
        let machine = view.directory.machine(&remote.machine);
        let machine = machine.map_or(remote.machine.as_str(), |machine| machine.name.as_str());
        return viewer::attach(ctx, remote.space.id.clone(), machine, session).await;
    }
    let target = Target::resolve(ctx, target).await?;
    // Attaching is the user's interaction with the space's agent.
    crate::client::agents::report_attach(&ctx.paths, SpaceSelector::id(target.id())).await?;
    let (target, session) = target.awake_session(ctx, session, None).await?;
    if terminal::attach(&target.number().to_string(), &session).await? {
        target.record_detach(ctx).await;
    }
    Ok(0)
}

/// The one of `target`'s `sessions` that `named` names, else the only one
/// [`offered`] offers, or one picked among them; for a user on another
/// machine, a refusal names `machine`, this one.
fn chosen_session<'a>(
    ctx: &Context,
    target: &Target,
    named: Option<String>,
    sessions: &'a [Session],
    machine: Option<&str>,
) -> Result<&'a Session> {
    if let Some(name) = named {
        return sessions
            .iter()
            .find(|session| session.name == name)
            .with_context(|| format!("{} has no session {name}", target.name()));
    }
    Ok(match offered(sessions).as_slice() {
        [] => bail!("{}", target.without_sessions(machine)),
        [only] => only,
        several => {
            let labels: Vec<_> = several.iter().map(|session| session.label()).collect();
            let choices: Vec<_> = several
                .iter()
                .zip(&labels)
                .map(|(session, label)| (*session, label.as_str()))
                .collect();
            ui::require_argument(ctx, "session", "session")?;
            ui::pick_choice(ctx, "Attach session> ", &choices)?
        }
    })
}

/// The ones of `sessions` an attach that names none offers: those where the
/// daemon recorded an agent, when any is, rather than a place such as one an
/// earlier run of the agent left, else every one.
fn offered(sessions: &[Session]) -> Vec<&Session> {
    let agents: Vec<_> = (sessions.iter())
        .filter(|session| session.agent.is_some())
        .collect();
    match agents.is_empty() {
        true => sessions.iter().collect(),
        false => agents,
    }
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

/// The spaces `attach -` returns to: every workspace and the chats whose
/// agent runs, since a chat whose agent exited has no session left.
fn attachable(spaces: Spaces) -> Spaces {
    (spaces.0.into_iter())
        .filter(|space| space.kind().has_worktree() || space.running)
        .collect()
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
        let spaces = crate::client::spaces::list(&ctx.paths).await?;
        if name.as_ref().is_some_and(recent::is_previous) {
            let id = recent::previous_among(ctx, &attachable(spaces.clone()))?;
            return Self::space(ctx, spaces.into_chats(), id).await;
        }
        let chats = spaces.into_chats();
        let running: Vec<Chat> = (chats.iter())
            .filter(|chat| chat.running)
            .cloned()
            .collect();
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
        let places = chat_places(&ctx.paths, &chats).await;
        let places: Vec<_> = places.iter().map(|(id, place)| (id, place)).collect();
        let attachable = terminal::session_owners(&places);
        let chats: Vec<_> = chats
            .into_iter()
            .filter(|chat| attachable.contains(&chat.id) || !chat.running || chat.asleep.is_some())
            .collect();
        let workspaces = crate::client::spaces::list(&ctx.paths)
            .await?
            .into_workspaces();
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
        let chats = crate::client::spaces::list(&ctx.paths).await?.into_chats();
        Self::space(ctx, chats, id).await
    }

    fn id(&self) -> &SpaceId {
        match self {
            Self::Workspace(inspection) => &inspection.workspace.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    /// Remember the space once its session was shown and still runs, so
    /// `attach -` returns to the session last detached from rather than the
    /// one last attached, which another terminal may still show.
    async fn record_detach(&self, ctx: &Context) {
        recent::record_attach(ctx, &SpaceSelector::id(self.id())).await;
    }

    /// The target's open sessions, each where an agent Shoal stopped waits
    /// marked with why.
    async fn sessions(&self, ctx: &Context) -> Result<Vec<Session>> {
        let settings = shoal_workers::settings::of(&ctx.paths, self.config()).await?;
        let space = SpaceSelector::id(self.id());
        let places = crate::client::workspaces::places(&ctx.paths, space).await?;
        let recorded = Recorded::space(&places.executions, &places.places);
        terminal::sessions(&settings, &recorded).await
    }

    /// The session `named` names, the target's only one or one picked,
    /// taken once the agent there runs. With no session named and none
    /// showing an agent, an agent that sleeps wakes first, and a stopped one
    /// when no session is left to attach to; a session where Shoal stopped
    /// the agent wakes it. For a user on another machine, a refusal names
    /// `machine`, this one.
    async fn awake_session(
        self,
        ctx: &Context,
        named: Option<String>,
        machine: Option<&str>,
    ) -> Result<(Self, Session)> {
        let sessions = self.sessions(ctx).await?;
        let shows_agent = sessions.iter().any(|session| session.agent.is_some());
        if named.is_none() && !shows_agent && self.wake(ctx, sessions.is_empty()).await? {
            return self.awaited(ctx, None).await;
        }
        let chosen = chosen_session(ctx, &self, named, &sessions, machine)?.clone();
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
    /// that sleeps, or when `stopped` too restore its stopped work now.
    /// Whether it woke one.
    async fn wake(&self, ctx: &Context, stopped: bool) -> Result<bool> {
        if super::queue::resume_space(ctx, self.id()).await? {
            return Ok(true);
        }
        if !stopped || !self.has_stopped_work(ctx).await? {
            return Ok(false);
        }
        self.restore_now(ctx).await?;
        Ok(true)
    }

    /// Whether the target saved stopped work or a last session to restore.
    async fn has_stopped_work(&self, ctx: &Context) -> Result<bool> {
        let saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(self.id())).await?;
        Ok(!saved.is_empty())
    }

    /// Restore the target's stopped work as `shoal restore --now` does, and
    /// wait for the restore it starts through the queue; refused when the
    /// restore reports that it failed, which it has reported already.
    async fn restore_now(&self, ctx: &Context) -> Result<()> {
        let targets = SpaceTargets {
            space: vec![SpaceSelector::id(self.id())],
            all: false,
        };
        let now = super::resume::Options {
            execution: None,
            forget: false,
            no_resume: false,
            now: true,
            focus: false,
            change: None,
        };
        let code = super::restore::run(ctx, targets, None, now).await?;
        ensure!(
            code == 0,
            "{} did not restore; shoal alert list shows why",
            self.name()
        );
        // A restore whose agent registered already left the queue, which
        // `awaited` finds running.
        super::queue::resume_space(ctx, self.id()).await?;
        Ok(())
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

    fn name(&self) -> &shoal_protocol::space_names::SpaceName {
        match self {
            Self::Workspace(inspection) => &inspection.workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }

    fn number(&self) -> shoal_ids::space_ids::SpaceNumber {
        match self {
            Self::Workspace(inspection) => inspection.workspace.number,
            Self::Chat(chat) => chat.number,
        }
    }

    /// The target's session at `place`, as an attach that lost its
    /// connection finds it again, without waking or restoring its agent.
    async fn session_again(
        self,
        ctx: &Context,
        place: &crate::sessions::Origin,
    ) -> Result<(Self, Session)> {
        let sessions = self.sessions(ctx).await?;
        let found = sessions
            .into_iter()
            .find(|session| session.place.same_place(place));
        let Some(session) = found else {
            bail!("{}'s session is gone", self.name());
        };
        if let Some(protection) = &session.stopped {
            bail!("{}", stopped_agent(&self, protection));
        }
        Ok((self, session))
    }

    /// Why there is nothing to attach to, and for a chat where it runs
    /// instead; for a user on another machine, that it runs on `machine`.
    fn without_sessions(&self, machine: Option<&str>) -> String {
        let without = self.without_sessions_here();
        match machine {
            Some(machine) => format!(
                "{without}; it runs on machine {machine}: attach there, or move it to your machine with shoal move"
            ),
            None => without,
        }
    }

    fn without_sessions_here(&self) -> String {
        match self {
            Self::Workspace(inspection) => {
                format!("{} has no agent sessions", inspection.workspace.name)
            }
            Self::Chat(chat) => match chat.agent.as_ref().and_then(agents::chat_place) {
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

/// Where the daemon recorded `chats` running, by chat ID: where their agents
/// run and the places Shoal created for them, as a workspace's. A list the
/// daemon cannot give counts as no place.
pub(in crate::cli) async fn chat_places(
    paths: &CommandPaths,
    chats: &[Chat],
) -> Vec<(SpaceId, Origin)> {
    let agents = crate::client::agents::all(paths).await.unwrap_or_default();
    let owned = crate::client::workspaces::owned_places(paths)
        .await
        .unwrap_or_default();
    let running = (agents.into_iter()).filter_map(|agent| Some((agent.space_id, agent.origin?)));
    let created = (owned.into_iter()).map(|place| (place.space_id, place.origin));
    running
        .chain(created)
        .filter(|(id, _)| chats.iter().any(|chat| chat.id == *id))
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
