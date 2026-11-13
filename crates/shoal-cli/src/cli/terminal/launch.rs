//! Starting Shoal work beside this terminal: additions, restores, tracked
//! agents and chats, focused or in the background.
use crate::client::space_names::{SpaceName, SpaceSelector};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

use super::sessions::{left_open, report_detach};
use crate::{
    cli::{commands::issues::Issue, context::Context, recent},
    config::{Effective, modes::ModeName},
    env,
    execution::TrackedAgent,
    model::{Repository, Workspace},
    sessions::Origin,
    start_plan::AddPlan,
    terminal::{
        Attaches, Configured, Owner, Places, Start, chat_place_env, current, groups, name_plan,
        open_add, place_env, rerun_in_host, shoal_command, show_opened, start_in_background,
    },
};

/// Start `plan` beside this terminal: focused when it waits for the user,
/// otherwise in the background, unless its mode's or the terminal's `focus`
/// setting says otherwise.
/// Returns whether it left this process; a focused launch without a new tab
/// stays here, where its agent runs in a session this terminal attaches to.
/// `name` is the planned workspace name; `focus` focuses the start whatever
/// the settings say.
pub(in crate::cli) async fn start_add(
    ctx: &Context,
    settings: &Effective,
    focus: bool,
    plan: &mut AddPlan,
    issue: Option<&Issue>,
    name: &str,
) -> Result<bool> {
    ensure!(
        !env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot allocate workspaces"
    );
    let host = beside(ctx, settings);
    let requested = focus.then_some(Placement::Focus);
    let placement = Placement::of(settings, &plan.mode(), requested, !plan.runs_unattended());
    let issue = issue.map(Issue::place_issue);
    name_plan(&ctx.paths, host.options.name_template.clone(), plan, issue).await?;
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
    open_add(&ctx.paths, &host, places, beside, plan, name, focus).await?;
    if !focus && !ctx.json {
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
    let repo = groups::Repo::Id(workspace.repository_id.as_str());
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

/// Run a tracked agent's `command` apart from this process, where the host
/// for this terminal runs agents so, and show it here. `None` means the
/// agent runs in place in this process. `records` are recovery records the
/// launch consumes once it starts.
pub(in crate::cli) async fn run_agent(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    agent: &TrackedAgent,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<Option<i32>> {
    let current = current(settings);
    let Some(run) = AgentRun::of(&current) else {
        return Ok(None);
    };
    let repo = groups::Repo::Id(workspace.repository_id.as_str());
    let origin = groups::beside(&ctx.paths, &current, &repo, &workspace.path).await?;
    let beside = current.beside(origin.as_ref());
    let worker = agent_worker(ctx, &current, workspace, agent, records, command)?;
    let mut env = place_env(&ctx.paths)?;
    if let Some(entry) = env::queue_entry() {
        env.push((env::QUEUE_ENTRY, entry.to_string()));
    }
    let start = Start {
        state: &ctx.paths.state,
        origin: beside.origin,
        cwd: &workspace.path,
        label: &workspace.name,
        focus: true,
        split: beside.split,
        argv: &worker,
        env: &env,
    };
    let left_running = match run {
        AgentRun::Session(attaches) => {
            let owner = Owner::Workspace(&workspace.id);
            // A session is named after the directory, which never changes.
            let start = Start {
                label: workspace.directory_name(),
                ..start
            };
            let session = attaches.run_agent(&start, owner, &agent.name).await?;
            report_detach(attaches, &workspace.number.to_string(), &session).await?
        }
        AgentRun::Place(places) => {
            let place = places.open(&start).await?;
            places.show(&place, &workspace.name).await?;
            left_open(&place).await
        }
    };
    if left_running {
        recent::record_attach(ctx, &SpaceSelector::id(&workspace.id)).await;
    }
    Ok(Some(0))
}

/// The worker that runs `agent`'s tracked `command` for `workspace` in a
/// place `host` opens, closing it with the workspace when config says so.
fn agent_worker(
    ctx: &Context,
    host: &Configured,
    workspace: &Workspace,
    agent: &TrackedAgent,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<Vec<OsString>> {
    let name = host.host.name();
    crate::workers::internal_command(
        &ctx.paths,
        false,
        crate::workers::Worker::Session {
            workspace: &workspace.id,
            agent,
            records,
            command,
            host: name,
            close_in: host.closes_when_done.then_some(name),
        },
    )
}

/// Where [`start_chat`] started a chat.
pub(in crate::cli) enum ChatStarted {
    /// Nowhere: the chat runs in place.
    Here,
    /// In a place shown here, or a session this terminal attached to.
    Shown,
    /// In a background place, as the words that report it say.
    Background(String),
}

/// Run Shoal with `args` for the chat `name` of `repository`, or of none,
/// in `cwd`, its checkout, worktree or `root_dir`, as `placement` asks: in
/// a new background place, or focused in a new session this terminal
/// attaches to, as `run_agent` does for tracked agents, otherwise in a new
/// focused place, shown here when its host does not serve this terminal.
pub(in crate::cli) async fn start_chat(
    ctx: &Context,
    settings: &Effective,
    repository: Option<&Repository>,
    cwd: &Path,
    name: &SpaceName,
    placement: Placement,
    args: &[OsString],
) -> Result<ChatStarted> {
    let host = beside(ctx, settings);
    let Some(places) = host.places() else {
        return Ok(ChatStarted::Here);
    };
    let repo = groups::Repo::of_chat(repository, cwd);
    let origin = groups::beside(&ctx.paths, &host, &repo, cwd).await?;
    let beside = host.beside(origin.as_ref());
    let focus = placement == Placement::Focus;
    let attaches = host.host.attaches().filter(|_| focus);
    let mut env = chat_place_env(&ctx.paths)?;
    // A chat registers only once the place runs it, so the chat itself
    // records the place it is shown in for `attach -`.
    if focus && attaches.is_none() {
        env.push((env::SHOWN_CHAT, "1".to_owned()));
    }
    let start = Start {
        state: &ctx.paths.state,
        origin: beside.origin,
        cwd,
        label: name,
        focus,
        split: beside.split,
        argv: &shoal_command(&ctx.paths, args)?,
        env: &env,
    };
    if let Some(attaches) = attaches {
        let session = attaches.run_chat(&start).await?;
        let chat = recent::session_chat(ctx, host.host.name(), &session).await;
        // Before the chat records its session, `-` is where it returns to.
        let space = chat.as_ref().map(|chat| chat.number.to_string());
        let space = space.as_deref().unwrap_or(SpaceSelector::PREVIOUS);
        if report_detach(attaches, space, &session).await?
            && let Some(chat) = chat
        {
            recent::record_chat_attach(ctx, &SpaceSelector::id(&chat.id)).await;
        }
        return Ok(ChatStarted::Shown);
    }
    let place = places.open(&start).await?;
    match placement {
        Placement::Focus => {
            show_opened(&host, places, &place, name).await?;
            Ok(ChatStarted::Shown)
        }
        Placement::Background => Ok(ChatStarted::Background(format!(
            "Started {name} {}",
            places.background_place(name)
        ))),
    }
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
