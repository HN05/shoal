//! Starting Shoal work beside this terminal: additions, restores, tracked
//! agents and chats, focused or in the background.
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::space_names::SpaceSelector;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use super::sessions::{left_open, report_detach};
use crate::{
    cli::{context::Context, recent},
    config::{Effective, modes::ModeName},
    env,
    execution::TrackedAgent,
    model::{Chat, Part, Repository, Space, Workspace},
    sessions::Origin,
    start_plan::AddPlan,
    terminal::{
        Attaches, Configured, Owner, Places, Start, chat_place_env, chat_session_name, current,
        open_add, place_env, rerun_in_host, shoal_command, show_opened,
    },
};
use shoal_workers::places::{groups, naming::name_plan, queued::start_in_background};

/// Start `plan` in `workspace` beside this terminal as its start placed
/// it, focused or in the background. Returns whether it left this process;
/// a focused launch without a new tab stays here, where its agent runs in a
/// session this terminal attaches to.
pub(in crate::cli) async fn start_add(
    ctx: &Context,
    settings: &Effective,
    placement: Placement,
    plan: &mut AddPlan,
    workspace: &Workspace,
) -> Result<bool> {
    ensure!(
        !env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot allocate workspaces"
    );
    let host = beside(ctx, settings);
    name_plan(&ctx.paths, host.options.name_template.clone(), plan, None).await?;
    let Some(places) = host.places() else {
        return Ok(false);
    };
    // A focused start stays here where the host keeps agents in sessions
    // this terminal attaches to, so its agent starts in one.
    let focus = placement == Placement::Focus;
    if focus && host.host.attaches().is_some() {
        return Ok(false);
    }
    let cwd = std::env::current_dir()?;
    let repo = groups::Repo::Selected(plan.repository.clone());
    let origin = groups::beside(&ctx.paths, &host, &repo, &cwd).await?;
    let beside = host.beside(origin.as_ref());
    open_add(&ctx.paths, &host, places, beside, plan, workspace, focus).await?;
    if !focus && !ctx.json {
        let name = workspace.name.as_str();
        outln!("Started {name} {}", places.background_place(name));
    }
    Ok(true)
}

/// Run Shoal with `args` where an agent stopped, in `stopped`, when that
/// place is another one still open and idle. Returns what such restarts run
/// in, or `None` when it did not run.
pub(in crate::cli) async fn rerun_where_stopped(
    ctx: &Context,
    settings: &Effective,
    stopped: Option<&Origin>,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    let host = beside(ctx, settings).host;
    let reused = rerun_in_host(&ctx.paths, host, stopped, args).await?;
    Ok(host
        .places()
        .filter(|_| reused)
        .map(Places::background_places))
}

/// Run Shoal with `args` for `workspace` in the background, returning what
/// such starts run in, or `None` when this terminal cannot start them.
pub(in crate::cli) async fn run_in_background(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    let host = beside(ctx, settings);
    let Some(places) = host.places() else {
        return Ok(None);
    };
    let repo = groups::Repo::Id(workspace.repository().as_str());
    let origin = groups::beside(&ctx.paths, &host, &repo, &workspace.path).await?;
    let beside = host.beside(origin.as_ref());
    start_in_background(&ctx.paths, &host, places, beside, workspace, args).await?;
    Ok(Some(places.background_places()))
}

/// Where a tracked agent launched from this terminal runs apart from this
/// process.
enum AgentRun {
    /// In a new session this terminal attaches to.
    Session(&'static dyn Attaches),
    /// In a new place of a host that does not serve this terminal, shown
    /// here.
    Place(&'static dyn Places),
}

impl AgentRun {
    /// Where `host` runs the agent, or `None` for in place, as in a place
    /// of a host serving this terminal.
    fn of(host: &Configured) -> Option<Self> {
        if let Some(attaches) = host.host.attaches() {
            return Some(Self::Session(attaches));
        }
        host.places().filter(|_| !host.serves()).map(Self::Place)
    }
}

/// The space a tracked agent runs for, as a host places the agent: beside
/// the spaces of its repository, in its directory, under its name.
pub(in crate::cli) struct AgentSpace<'a> {
    /// The repository whose spaces its place opens beside, or the
    /// `root_dir` of a chat without one.
    repo: groups::Repo<'a>,
    /// Where the agent works.
    directory: &'a Path,
    /// The name its place shows.
    label: &'a str,
    /// The name of a session it runs in, which never changes.
    session: String,
    /// The number `shoal attach` takes it by.
    number: SpaceNumber,
    owner: Owner<'a>,
}

impl<'a> AgentSpace<'a> {
    /// What a host reads of `space`, a workspace or a chat. A workspace's
    /// session is named after its worktree directory.
    pub(in crate::cli) fn of<P: Part>(space: &'a Space<P>) -> Self {
        let directory = space.workdir.path();
        let repo = match &space.repository_id {
            Some(id) => groups::Repo::Id(id.as_str()),
            None => groups::Repo::Root(directory),
        };
        let (owner, session) = match space.worktree() {
            Some(worktree) => (
                Owner::Workspace(&space.id),
                worktree.directory_name().to_owned(),
            ),
            None => (Owner::Chat(&space.id), chat_session_name(&space.id)),
        };
        Self {
            repo,
            directory,
            label: &space.name,
            session,
            number: space.number,
            owner,
        }
    }

    /// Whom the agent's session or place is for.
    pub(in crate::cli) fn owner(&self) -> Owner<'a> {
        self.owner
    }
}

/// Run a tracked agent's `command` for `space` apart from this process,
/// where the host for this terminal runs agents so, and show it here.
/// `None` means the agent runs in place in this process. `records` are
/// recovery records the launch consumes once it starts.
pub(in crate::cli) async fn run_agent(
    ctx: &Context,
    settings: &Effective,
    space: &AgentSpace<'_>,
    agent: &TrackedAgent,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<Option<i32>> {
    let current = current(settings);
    let Some(run) = AgentRun::of(&current) else {
        return Ok(None);
    };
    let origin = groups::beside(&ctx.paths, &current, &space.repo, space.directory).await?;
    let beside = current.beside(origin.as_ref());
    let worker = agent_worker(ctx, &current, space.owner, agent, records, command)?;
    let mut env = place_env(&ctx.paths)?;
    if let Some(entry) = env::queue_entry() {
        env.push((env::QUEUE_ENTRY, entry.to_string()));
    }
    let start = Start {
        state: &ctx.paths.state,
        origin: beside.origin,
        cwd: space.directory,
        label: space.label,
        focus: true,
        split: beside.split,
        argv: &worker,
        env: &env,
    };
    let left_running = match run {
        AgentRun::Session(attaches) => {
            let start = Start {
                label: &space.session,
                ..start
            };
            let session = attaches.run_agent(&start, space.owner, &agent.name).await?;
            report_detach(attaches, &space.number.to_string(), &session).await?
        }
        AgentRun::Place(places) => {
            let place = places.open(&start).await?;
            places.show(&place, space.label).await?;
            left_open(&place).await
        }
    };
    if left_running {
        let id = space.owner.id();
        recent::record_attach(ctx, &SpaceSelector::id(id)).await;
    }
    Ok(Some(0))
}

/// The worker that runs `agent`'s tracked `command` for `owner` in a place
/// `host` opens, closing a workspace's with the workspace when config says
/// so; a chat's goes with the chat's removal.
fn agent_worker(
    ctx: &Context,
    host: &Configured,
    owner: Owner<'_>,
    agent: &TrackedAgent,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<Vec<OsString>> {
    let name = host.host.name();
    let closes = host.closes_when_done && matches!(owner, Owner::Workspace(_));
    crate::workers::internal_command(
        &ctx.paths,
        false,
        crate::workers::Worker::Session {
            space: owner.id(),
            agent,
            records,
            command,
            host: name,
            close_in: closes.then_some(name),
        },
    )
}

/// Where [`start_chat`] started a chat.
pub(in crate::cli) enum ChatStarted {
    /// In a place shown here, or a session this terminal attached to.
    Shown,
    /// In a background place, as the words that report it say.
    Background(String),
}

/// Run Shoal with `args` for the recorded `chat` of `repository`, or of
/// none, in `cwd`, its checkout, worktree or `root_dir`, as `placement`
/// asks: in a new background place, or focused in a new session this
/// terminal attaches to, as `run_agent` does for tracked agents, otherwise
/// in a new focused place, shown here when its host does not serve this
/// terminal. A chat shown here becomes the one `attach -` returns to. Only
/// a host that [`opens_beside`] this terminal starts one.
pub(in crate::cli) async fn start_chat(
    ctx: &Context,
    settings: &Effective,
    (repository, cwd): (Option<&Repository>, &Path),
    chat: &Chat,
    placement: Placement,
    args: &[OsString],
) -> Result<ChatStarted> {
    let host = beside(ctx, settings);
    let places = (host.places()).context("no host opens a place beside this terminal")?;
    let repo = groups::Repo::of_chat(repository, cwd);
    let origin = groups::beside(&ctx.paths, &host, &repo, cwd).await?;
    let beside = host.beside(origin.as_ref());
    let focus = placement == Placement::Focus;
    let start = Start {
        state: &ctx.paths.state,
        origin: beside.origin,
        cwd,
        label: &chat.name,
        focus,
        split: beside.split,
        argv: &shoal_command(&ctx.paths, args)?,
        env: &chat_place_env(&ctx.paths)?,
    };
    let shown = SpaceSelector::id(&chat.id);
    if let Some(attaches) = host.host.attaches().filter(|_| focus) {
        let session = attaches.run_chat(&start).await?;
        if report_detach(attaches, &chat.number.to_string(), &session).await? {
            recent::record_attach(ctx, &shown).await;
        }
        return Ok(ChatStarted::Shown);
    }
    let place = places.open(&start).await?;
    match placement {
        Placement::Focus => {
            show_opened(&host, places, &place, &chat.name).await?;
            recent::record_attach(ctx, &shown).await;
            Ok(ChatStarted::Shown)
        }
        Placement::Background => Ok(ChatStarted::Background(format!(
            "Started {} {}",
            chat.name,
            places.background_place(&chat.name)
        ))),
    }
}

/// Whether the host under `settings` opens places beside this terminal for
/// the starts made from it.
pub(in crate::cli) fn opens_beside(ctx: &Context, settings: &Effective) -> bool {
    beside(ctx, settings).places().is_some()
}

/// The host that starts work beside this terminal. A place a host opened for
/// this process, or a terminal without a user, keeps work in place; a call
/// that asked for JSON keeps its terminal's host.
fn beside(ctx: &Context, settings: &Effective) -> Configured {
    if ctx.place.is_none() && ctx.on_terminal() {
        current(settings)
    } else {
        Configured::plain(settings)
    }
}

/// Whether a launch takes the caller's terminal or runs in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Placement {
    Focus,
    Background,
}

impl Placement {
    /// Where a launch in `mode` goes under `settings`: where it was
    /// `requested`, else as its `focus` setting says, else focused when it
    /// waits for the user.
    pub(in crate::cli) fn of(
        settings: &Effective,
        mode: &ModeName,
        requested: Option<Self>,
        waits_for_user: bool,
    ) -> Self {
        requested.unwrap_or_else(|| Self::choose(settings.focus(mode), waits_for_user))
    }

    /// The `focus` setting, otherwise focus a launch that waits for the user.
    fn choose(setting: Option<bool>, waits_for_user: bool) -> Self {
        if setting.unwrap_or(waits_for_user) {
            Self::Focus
        } else {
            Self::Background
        }
    }
}

#[cfg(test)]
mod tests;
