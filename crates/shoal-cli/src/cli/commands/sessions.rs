//! `shoal attach`, and what the CLI does its own way in the attach steps
//! [`shoal_workers::attach`] takes.

use crate::cli::ui::Acting;
use anyhow::{Result, ensure};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::attaching::Viewer;
use shoal_workers::attach::{Attaching, Target};

use super::viewer::{self, Viewed};
use crate::{
    cli::{
        context::Context,
        recent,
        ui::{self, Fallback, NoCurrentWorkspace},
    },
    client::connection::CommandPaths,
    model::{Chat, QueueEntry, Spaces},
    sessions::Origin,
    state::QueueKind,
    terminal::Session,
};
use shoal_protocol::{
    hub::directory::{HubView, Offered},
    model::Located,
    space_names::{SpaceName, SpaceSelector},
};

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
    let target = match resolve(ctx, target).await? {
        Chosen::Here(target) => target,
        Chosen::Elsewhere { space, machine } => {
            return view_elsewhere(ctx, space, &machine, session).await;
        }
    };
    let id = SpaceSelector::id(target.id());
    if shoal_workers::attach::here(&ctx.paths, target, session, &Callbacks(ctx)).await? {
        // Remember the space once its session was shown and still runs, so
        // `attach -` returns to the session last detached from rather than
        // the one last attached, which another terminal may still show.
        shoal_workers::history::record_attach(&ctx.paths, &id).await;
    }
    Ok(0)
}

/// Remembered for `attach -` once its session was shown and still runs there.
async fn view_elsewhere(
    ctx: &Context,
    space: SpaceId,
    machine: &str,
    session: Option<String>,
) -> Result<i32> {
    let viewed = Viewed::session(space.clone(), session).await;
    let ended = viewer::view(ctx, viewed, machine).await?;
    if ended.open {
        shoal_workers::history::record_attach_elsewhere(&ctx.paths, &space);
    }
    Ok(ended.code)
}

pub(in crate::cli::commands) async fn attach_started(
    ctx: &Context,
    space: SpaceId,
    machine: Option<&str>,
) -> Result<i32> {
    match machine {
        Some(machine) => view_elsewhere(ctx, space, machine, None).await,
        None => attach(ctx, Some(SpaceSelector::id(&space)), None).await,
    }
}

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

    fn stopped_commands(
        &self,
        (number, name): (SpaceNumber, &SpaceName),
        commands: &[Vec<String>],
    ) {
        super::resume::reports::stopped_commands(self.0, (number, name, None), commands);
    }

    fn resuming(&self, entry: &QueueEntry) -> Result<()> {
        let starts = match entry.kind {
            QueueKind::Launch => "Starting",
            QueueKind::Restore | QueueKind::Job => "Resuming",
        };
        self.0.emit(&format!("{starts} {}", entry.name), entry)
    }
}

/// A chat whose agent exited has no session left.
fn attachable(spaces: Spaces) -> Spaces {
    (spaces.0.into_iter())
        .filter(|space| space.kind().has_worktree() || space.running)
        .collect()
}

enum Chosen {
    Here(Target),
    Elsewhere { space: SpaceId, machine: String },
}

/// `-` is the space whose session this terminal last left running.
async fn resolve(ctx: &Context, name: Option<SpaceSelector>) -> Result<Chosen> {
    if name.as_ref().is_some_and(recent::is_previous) {
        return previous(ctx).await;
    }
    if let Some(selector) = &name
        && let Located::Elsewhere { space, machine, .. } =
            crate::client::spaces::locate(&ctx.paths, selector.clone()).await?
    {
        return Ok(chosen_elsewhere(space.space.id, machine.name));
    }
    resolve_here(ctx, name).await
}

fn chosen_elsewhere(space: SpaceId, machine: String) -> Chosen {
    Chosen::Elsewhere { space, machine }
}

/// The space whose session this terminal last left running, other than
/// the current one, on this machine or another joined one.
async fn previous(ctx: &Context) -> Result<Chosen> {
    let everywhere = crate::cli::everywhere::spaces(ctx).await?;
    let (spaces, view) = (everywhere.here, everywhere.elsewhere);
    let elsewhere = view.as_ref().map(HubView::attachable).unwrap_or_default();
    let id = recent::previous_among(ctx, &attachable(spaces.clone()), &elsewhere)?;
    let listed = (view.as_ref()).and_then(|view| {
        let listed = view.listed(&id)?;
        Some(chosen_elsewhere(
            id.clone(),
            view.machine_name(listed).to_owned(),
        ))
    });
    match listed {
        Some(chosen) => Ok(chosen),
        None => Ok(Chosen::Here(
            Target::among(&ctx.paths, spaces.into_chats(), id).await?,
        )),
    }
}

async fn resolve_here(ctx: &Context, name: Option<SpaceSelector>) -> Result<Chosen> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let chats = spaces.into_chats();
    let running: Vec<Chat> = (chats.iter())
        .filter(|chat| chat.running)
        .cloned()
        .collect();
    if let Some(name) = name {
        let target = match (chats.into_iter()).find(|chat| name.selects(&chat.id, chat.number)) {
            Some(chat) => Target::Chat(Box::new(chat)),
            // A named workspace needs no picker.
            None => workspace(ctx, Some(name), Fallback::CurrentDirectoryOnly).await?,
        };
        return Ok(Chosen::Here(target));
    }
    match workspace(ctx, None, Fallback::CurrentDirectoryOnly).await {
        Err(error) if error.is::<NoCurrentWorkspace>() => {}
        current => return current.map(Chosen::Here),
    }
    if let Some(chat) = checkout_chat(ctx, &running).await? {
        return Ok(Chosen::Here(Target::Chat(Box::new(chat))));
    }
    picked(ctx, chats).await
}

async fn workspace(
    ctx: &Context,
    name: Option<SpaceSelector>,
    fallback: Fallback<'_>,
) -> Result<Target> {
    let workspace = ui::choose_workspace(ctx, name, fallback).await?;
    Target::inspected(&ctx.paths, workspace).await
}

async fn picked(ctx: &Context, chats: Vec<Chat>) -> Result<Chosen> {
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
    let id = ui::pick_space(
        ctx,
        workspaces,
        &chats,
        Offered::Spaces,
        "workspace",
        &attaching(),
    )
    .await?;
    match crate::client::spaces::locate(&ctx.paths, SpaceSelector::id(&id)).await? {
        Located::Elsewhere { space, machine, .. } => {
            Ok(chosen_elsewhere(space.space.id, machine.name))
        }
        Located::Here(_) => Ok(Chosen::Here(Target::among(&ctx.paths, chats, id).await?)),
    }
}

/// A list the daemon cannot give counts as no place.
pub(in crate::cli) async fn chat_places(
    paths: &CommandPaths,
    chats: &[Chat],
) -> Vec<(SpaceId, Origin)> {
    let agents = crate::client::agents::all(paths).await.unwrap_or_default();
    let owned = crate::client::places::owned_places(paths)
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

pub(super) fn attaching() -> Acting {
    Acting::request(|space| crate::client::attaching::attach_request(space, Default::default()))
}
