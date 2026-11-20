//! `shoal attach`: choosing the space to attach to, and what the CLI does
//! its own way in the attach steps `shoal_workers::attach` takes, which
//! hand this terminal to one of a workspace's or a chat's sessions. A user
//! on another machine attaches through a terminal the space's machine
//! makes for them.
mod viewer;

use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::attaching::Viewer;
use shoal_workers::attach::{Attaching, Target};

use crate::{
    cli::{
        args::SpaceTargets,
        context::Context,
        recent,
        ui::{self, Fallback, NoCurrentWorkspace},
    },
    client::connection::CommandPaths,
    model::{Chat, QueueEntry, Spaces},
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
    let target = resolve(ctx, target).await?;
    let id = SpaceSelector::id(target.id());
    if shoal_workers::attach::here(&ctx.paths, target, session, &Callbacks(ctx)).await? {
        // Remember the space once its session was shown and still runs, so
        // `attach -` returns to the session last detached from rather than
        // the one last attached, which another terminal may still show.
        recent::record_attach(ctx, &id).await;
    }
    Ok(0)
}

/// `shoal internal attach`: attach this terminal, which `viewer` views
/// from another machine, to a session of `space` on `machine`, this one,
/// reporting the session chosen to the descriptor `report`.
pub(in crate::cli::commands) async fn attach_for_viewer(
    ctx: &Context,
    space: SpaceId,
    viewer: Viewer,
    machine: String,
    report: i32,
) -> Result<i32> {
    let callbacks = Callbacks(ctx);
    shoal_workers::attach::for_viewer(&ctx.paths, space, viewer, machine, report, &callbacks).await
}

/// What the CLI does its own way in an attach's steps.
struct Callbacks<'a>(&'a Context);

impl Attaching for Callbacks<'_> {
    fn pick_session<'s>(&self, sessions: &[&'s Session]) -> Result<&'s Session> {
        let labels: Vec<_> = sessions.iter().map(|session| session.label()).collect();
        let choices: Vec<_> = (sessions.iter().copied())
            .zip(&labels)
            .map(|(session, label)| (session, label.as_str()))
            .collect();
        ui::require_argument(self.0, "session", "session")?;
        ui::pick_choice(self.0, "Attach session> ", &choices)
    }

    async fn resume_now(&self, space: &SpaceId) -> Result<bool> {
        let targets = SpaceTargets {
            space: vec![SpaceSelector::id(space)],
            all: false,
        };
        let now = super::resume::Options {
            execution: None,
            now: true,
            focus: false,
            change: None,
        };
        Ok(super::resume::run(self.0, targets, now).await? == 0)
    }

    fn resuming(&self, entry: &QueueEntry) -> Result<()> {
        self.0.emit(&format!("Resuming {}", entry.name), entry)
    }
}

/// The spaces `attach -` returns to: every workspace and the chats whose
/// agent runs, since a chat whose agent exited has no session left.
fn attachable(spaces: Spaces) -> Spaces {
    (spaces.0.into_iter())
        .filter(|space| space.kind().has_worktree() || space.running)
        .collect()
}

/// The chat or workspace `name` selects, where `-` is the one whose session
/// this terminal last left running; without one, the current workspace,
/// else the current checkout's only running chat, else a picker of the
/// workspaces and the chats with sessions or an agent that sleeps or is
/// stopped.
async fn resolve(ctx: &Context, name: Option<SpaceSelector>) -> Result<Target> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    if name.as_ref().is_some_and(recent::is_previous) {
        let id = recent::previous_among(ctx, &attachable(spaces.clone()))?;
        return Target::among(&ctx.paths, spaces.into_chats(), id).await;
    }
    let chats = spaces.into_chats();
    let running: Vec<Chat> = (chats.iter())
        .filter(|chat| chat.running)
        .cloned()
        .collect();
    if let Some(name) = name {
        return match (chats.into_iter()).find(|chat| name.selects(&chat.id, chat.number)) {
            Some(chat) => Ok(Target::Chat(Box::new(chat))),
            None => workspace(ctx, Some(name), Fallback::CurrentDirectory).await,
        };
    }
    match workspace(ctx, None, Fallback::CurrentDirectoryOnly).await {
        Err(error) if error.is::<NoCurrentWorkspace>() => {}
        current => return current,
    }
    if let Some(chat) = checkout_chat(ctx, &running).await? {
        return Ok(Target::Chat(Box::new(chat)));
    }
    picked(ctx, chats).await
}

async fn workspace(
    ctx: &Context,
    name: Option<SpaceSelector>,
    fallback: Fallback,
) -> Result<Target> {
    let workspace = ui::choose_workspace(ctx, name, fallback).await?;
    Target::inspected(&ctx.paths, workspace).await
}

/// A workspace or one of `chats` with an agent session, or whose agent
/// sleeps or is stopped, which the picker's status shows, from a picker.
async fn picked(ctx: &Context, chats: Vec<Chat>) -> Result<Target> {
    let places = chat_places(&ctx.paths, &chats).await;
    let places: Vec<_> = places.iter().map(|(id, place)| (id, place)).collect();
    let attachable = crate::terminal::session_owners(&places);
    let chats: Vec<_> = chats
        .into_iter()
        .filter(|chat| attachable.contains(&chat.id) || !chat.running || chat.asleep.is_some())
        .collect();
    let workspaces = crate::client::spaces::list(&ctx.paths)
        .await?
        .into_workspaces();
    let id = ui::pick_space(ctx, workspaces, &chats, "workspace").await?;
    Target::among(&ctx.paths, chats, id).await
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
