//! Choosing the spaces, workspaces or chats, a command acts on: the named
//! ones, the current one, or those from a picker, and what `shoal resume`
//! brings back in each.
use shoal_ids::space_ids::SpaceId;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, ensure};

use super::{
    Acting, Cell, Entries, chat_entries, elsewhere_entries, pick, pick_many, picker_width,
    require_argument, rows_with_status,
    workspaces::{
        ALL_ID, Fallback, NoCurrentWorkspace, Resolution, current_workspace, resolve_workspace,
        workspace_entries,
    },
};
use crate::client::output::resume::ResumableSpace;
use crate::{
    cli::{
        args::SpaceFilter,
        context::Context,
        list_status::{ListStatus, ListStatusStyle},
        output::Palette,
        recent,
    },
    client::connection::CommandPaths,
    model::{Chat, Workspace},
    scoping::Scoping,
    workspace_context::{ScopeOrder, WorkspaceContext},
};
use shoal_protocol::hub::directory::Offered;
use shoal_protocol::saved::SavedSummary;
use shoal_protocol::space_names::SpaceSelector;

/// The spaces, workspaces or chats, a command that acts on several takes:
/// those named, each once and in order, otherwise the current workspace,
/// otherwise those picked among the workspaces and the running chats, or
/// every chat when `stopped_chats`, of this machine and, for a command that
/// acts on them, the other joined machines. It ends what it acts on, so it
/// never defaults to the chat it runs in. A name no workspace has stays as
/// given, for a chat or the daemon's refusal.
pub async fn select_spaces(
    ctx: &Context,
    names: Vec<SpaceSelector>,
    stopped_chats: bool,
    acting: &Acting,
) -> Result<Vec<SpaceSelector>> {
    let mut selected = named_workspaces(ctx, names).await?;
    if selected.is_empty() {
        selected = match resolve_workspace(ctx, None, Fallback::CurrentDirectory(acting)).await? {
            Resolution::Named(selector) => vec![selector],
            Resolution::Current(id) => vec![SpaceSelector::Id(id)],
            Resolution::Picker(workspaces, acting) => {
                let spaces = crate::client::spaces::list(&ctx.paths).await?;
                let (chats, offered) = match stopped_chats {
                    true => (spaces.into_chats(), Offered::Spaces),
                    false => (spaces.into_running_chats(), Offered::RunningChats),
                };
                require_argument(ctx, "spaces", SPACE)?;
                let entries = space_entries(ctx, workspaces, &chats, offered, acting).await;
                ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
                picked_ids(pick_many(ctx, "Spaces> ", entries)?)
            }
        };
    }
    for space in &selected {
        recent::record(ctx, space).await;
    }
    Ok(selected)
}

/// The spaces `names` select, as [`select_spaces`] takes named ones, never
/// offering a picker.
pub async fn named_spaces(ctx: &Context, names: Vec<SpaceSelector>) -> Result<Vec<SpaceSelector>> {
    let selected = named_workspaces(ctx, names).await?;
    for space in &selected {
        recent::record(ctx, space).await;
    }
    Ok(selected)
}

/// The workspaces `names` select, each once and in order by its ID, so a
/// workspace named both by its name and by its ID is acted on once. A name
/// no workspace has stays as given, for the daemon to refuse.
async fn named_workspaces(ctx: &Context, names: Vec<SpaceSelector>) -> Result<Vec<SpaceSelector>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let workspaces = crate::client::spaces::list(&ctx.paths)
        .await?
        .into_workspaces();
    let context = WorkspaceContext::from_directory(&workspaces, None);
    let mut selected = Vec::new();
    for name in names {
        let workspace = match recent::is_previous(&name) {
            true => SpaceSelector::Id(recent::previous(ctx).await?),
            false => (context.resolve(Some(&name), Scoping::Unscoped, ScopeOrder::AfterDirectory))
                .map_or(name, |workspace| SpaceSelector::id(&workspace.id)),
        };
        if !selected.contains(&workspace) {
            selected.push(workspace);
        }
    }
    Ok(selected)
}

/// The spaces `shoal resume` picks without a space: the current workspace
/// when it is among the `resumable` spaces, else those picked among them,
/// workspaces then chats. `None` without a terminal or anything to pick,
/// which the caller lists instead.
pub async fn select_resumable(
    ctx: &Context,
    resumable: &[ResumableSpace],
) -> Result<Option<Vec<SpaceSelector>>> {
    if let Some(id) = current_workspace_id(ctx).await?
        && resumable.iter().any(|space| space.space.id == id)
    {
        return Ok(Some(vec![SpaceSelector::Id(id)]));
    }
    let mut entries = resumable_workspace_entries(ctx, resumable);
    let chats: Vec<_> = (resumable.iter())
        .filter_map(|space| space.space.clone().into_chat().ok())
        .collect();
    entries.extend(chat_entries(
        &chats,
        Palette::stderr(ctx.json),
        picker_width(),
    ));
    if entries.is_empty() || !ctx.interactive() {
        return Ok(None);
    }
    pick_many(ctx, "Spaces> ", entries).map(|picked| Some(picked_ids(picked)))
}

/// The picker entries of the workspaces among `resumable`, in its order,
/// each with what of it resumes.
fn resumable_workspace_entries(ctx: &Context, resumable: &[ResumableSpace]) -> Entries {
    let (statuses, workspaces): (Vec<_>, Vec<_>) = (resumable.iter())
        .filter_map(|space| {
            let workspace = space.space.to_workspace()?;
            let (text, unfinished) = self::resumable(space);
            let status = ListStatus::of_workspace(&workspace, unfinished);
            Some(((status, resumable_status_cell(text, unfinished)), workspace))
        })
        .unzip();
    let palette = Palette::stderr(ctx.json);
    let labels = rows_with_status(&workspaces, &[], statuses, palette, picker_width(), false);
    workspaces
        .into_iter()
        .map(|w| w.id.into())
        .zip(labels)
        .collect()
}

/// What each space this process sees saved, in brief, by space; none when
/// the daemon cannot say, which only loses what the lists show.
pub async fn saved_summaries(paths: &CommandPaths) -> HashMap<SpaceId, SavedSummary> {
    let summaries = crate::client::saved::summaries(paths).await;
    let summaries = summaries.unwrap_or_default().into_iter();
    summaries
        .map(|summary| (summary.space.clone(), summary))
        .collect()
}

/// What `shoal resume` brings back in `space`, such as `stopped: claude,
/// 1 command` or `launch failed`, and whether it is unfinished work rather
/// than only last sessions.
pub fn resumable(space: &ResumableSpace) -> (String, bool) {
    match (&space.failed_launch, &space.saved) {
        (Some(_), _) => ("launch failed".into(), true),
        (None, saved) => saved_status(saved.as_ref()).unwrap_or_default(),
    }
}

/// What a space that saved `summary` resumes, such as `stopped: claude,
/// 1 command`, and whether it is unfinished work rather than only last
/// sessions; `None` when nothing is saved.
pub fn saved_status(summary: Option<&SavedSummary>) -> Option<(String, bool)> {
    let summary = summary?;
    let mut parts = (summary.agents.iter())
        .map(|agent| {
            agent
                .as_ref()
                .map_or_else(|| "agent".to_owned(), ToString::to_string)
        })
        .collect::<Vec<_>>();
    match summary.commands {
        0 => {}
        1 => parts.push("1 command".into()),
        n => parts.push(format!("{n} commands")),
    }
    Some((format!("stopped: {}", parts.join(", ")), summary.unfinished))
}

/// How completion names a workspace whose agent has nothing to resume.
pub const NOTHING_TO_RESUME: &str = "nothing to resume";

fn resumable_status_cell(text: String, unfinished: bool) -> Cell {
    match unfinished {
        true => (text, Some(ListStatus::Stopped.style())),
        false => (text, None),
    }
}

/// Pick a space among the workspaces and running chats, wherever this runs.
pub async fn pick_any_space(ctx: &Context, acting: &Acting) -> Result<SpaceSelector> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let workspaces = spaces.workspaces();
    let chats = spaces.into_running_chats();
    let offered = Offered::RunningChats;
    let space = pick_space(ctx, workspaces, &chats, offered, SPACE, acting).await?;
    Ok(SpaceSelector::Id(space))
}

/// The space, a workspace or chat, a command acts on: the one `explicit`
/// names, else in the chat this process runs in the workspace below the chat
/// that contains the current directory, as a workspace's process chooses one
/// below its own, then the chat, else the workspace `fallback` chooses, where
/// a picker offers the workspaces and running chats. A `fallback` of the
/// caller's own space takes the chat wherever its directory is.
pub async fn select_space(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback<'_>,
) -> Result<SpaceSelector> {
    match current_chat(&explicit) {
        Some(chat) if matches!(fallback, Fallback::OwnSpace) => Ok(SpaceSelector::Id(chat)),
        Some(chat) => current_workspace_or_chat(ctx, chat, Workspaces::Below).await,
        None => select_other_space(ctx, explicit, fallback).await,
    }
}

/// The space this runs in, never offering a picker: in a chat, the
/// workspace of `which` containing the current directory, else the chat;
/// otherwise the current workspace; none outside every space.
pub async fn space_here(ctx: &Context, which: Workspaces) -> Result<Option<SpaceSelector>> {
    if let Some(chat) = current_chat(&None) {
        return current_workspace_or_chat(ctx, chat, which).await.map(Some);
    }
    let Some(id) = current_workspace_id(ctx).await? else {
        return Ok(None);
    };
    let workspace = SpaceSelector::Id(id);
    recent::record(ctx, &workspace).await;
    Ok(Some(workspace))
}

/// The ID of the workspace the current directory is in, never offering a
/// picker; none outside every workspace.
async fn current_workspace_id(ctx: &Context) -> Result<Option<SpaceId>> {
    match resolve_workspace(ctx, None, Fallback::CurrentDirectoryOnly).await {
        Ok(Resolution::Current(id)) => Ok(Some(id)),
        Ok(Resolution::Named(_) | Resolution::Picker(..)) => Ok(None),
        Err(error) if error.is::<NoCurrentWorkspace>() => Ok(None),
        Err(error) => Err(error),
    }
}

/// Which workspaces containing the current directory come before the chat.
pub enum Workspaces {
    /// Those below the chat, which its scope covers.
    Below,
    /// Any, such as the one whose worktree the chat runs in.
    Any,
}

/// The workspace of `which` containing the current directory, else `chat`.
async fn current_workspace_or_chat(
    ctx: &Context,
    chat: SpaceId,
    which: Workspaces,
) -> Result<SpaceSelector> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let below = spaces.below(&chat);
    let workspaces = spaces.workspaces();
    let current = current_workspace(&workspaces)?.filter(|workspace| match which {
        Workspaces::Below => below.iter().any(|space| space.id == workspace.id),
        Workspaces::Any => true,
    });
    let Some(workspace) = current else {
        return Ok(SpaceSelector::Id(chat));
    };
    let workspace = SpaceSelector::id(&workspace.id);
    recent::record(ctx, &workspace).await;
    Ok(workspace)
}

/// [`select_space`] for a command that never acts on the chat it runs in,
/// such as one that ends the space it acts on: without one `explicit`, the
/// workspace `fallback` chooses or a picker decides.
pub async fn select_other_space(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback<'_>,
) -> Result<SpaceSelector> {
    let space = match resolve_workspace(ctx, explicit, fallback).await? {
        Resolution::Named(selector) => selector,
        Resolution::Current(id) => SpaceSelector::Id(id),
        Resolution::Picker(workspaces, acting) => {
            let chats = crate::client::spaces::list(&ctx.paths)
                .await?
                .into_running_chats();
            let offered = Offered::RunningChats;
            SpaceSelector::Id(pick_space(ctx, workspaces, &chats, offered, SPACE, acting).await?)
        }
    };
    recent::record(ctx, &space).await;
    Ok(space)
}

/// The chat this process runs in, outside any workspace, when no space is
/// `explicit`.
pub fn current_chat(explicit: &Option<SpaceSelector>) -> Option<SpaceId> {
    if explicit.is_some() {
        return None;
    }
    crate::env::own_chat()
}

/// [`select_space`] for a command that also acts on every space: `None`
/// when `--all` was passed or picked.
pub async fn select_space_filter(
    ctx: &Context,
    filter: SpaceFilter,
    acting: &Acting,
) -> Result<Option<SpaceSelector>> {
    if filter.all {
        return Ok(None);
    }
    if current_chat(&filter.space).is_some() {
        let space = select_space(ctx, filter.space, Fallback::CurrentDirectory(acting)).await?;
        return Ok(Some(space));
    }
    Ok(
        match resolve_workspace(ctx, filter.space, Fallback::CurrentDirectory(acting)).await? {
            Resolution::Named(selector) => Some(selector),
            Resolution::Current(id) => Some(SpaceSelector::Id(id)),
            Resolution::Picker(workspaces, acting) => {
                let chats = crate::client::spaces::list(&ctx.paths)
                    .await?
                    .into_running_chats();
                (pick_space_or_all(ctx, workspaces, &chats, acting).await?).map(SpaceSelector::Id)
            }
        },
    )
}

/// Pick a space as [`pick_space`] offers them, or every space after them:
/// `None` for that.
async fn pick_space_or_all(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    chats: &[Chat],
    acting: &Acting,
) -> Result<Option<SpaceId>> {
    let offered = Offered::RunningChats;
    let mut entries = space_entries(ctx, workspaces, chats, offered, acting).await;
    entries.push((ALL_ID.to_owned(), ALL_SPACES.to_owned()));
    require_argument(ctx, "space", SPACE)?;
    let id = pick(ctx, "Space> ", entries)?;
    Ok((id != ALL_ID).then(|| SpaceId::from(id)))
}

/// The command's argument that names the space it acts on.
const SPACE: &str = "space";

/// How the space picker names `--all`.
const ALL_SPACES: &str = "All spaces";

/// Pick a space: one of `workspaces`, or one of `chats`, listed after them
/// by name, or for a command that acts on them one of the other joined
/// machines' spaces `offered` names, for the command's `argument`. Returns
/// the chosen space's ID.
pub async fn pick_space(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    chats: &[Chat],
    offered: Offered,
    argument: &str,
    acting: &Acting,
) -> Result<SpaceId> {
    let entries = space_entries(ctx, workspaces, chats, offered, acting).await;
    ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
    require_argument(ctx, "space", argument)?;
    Ok(SpaceId::from(pick(ctx, "Space> ", entries)?))
}

/// The picker entries for `workspaces`, then the `chats` the command's need
/// offers, then the other machines' spaces `offered` names.
async fn space_entries(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    chats: &[Chat],
    offered: Offered,
    acting: &Acting,
) -> Entries {
    let mut entries = workspace_entries(ctx, workspaces, true).await;
    let chats: Vec<Chat> = (chats.iter())
        .filter(|chat| ctx.need.offers_chat(chat))
        .cloned()
        .collect();
    entries.extend(chat_entries(
        &chats,
        Palette::stderr(ctx.json),
        picker_width(),
    ));
    entries.extend(elsewhere_entries(ctx, offered, acting).await);
    entries
}

/// The spaces picked by the IDs their entries carry.
fn picked_ids(picked: Vec<String>) -> Vec<SpaceSelector> {
    picked
        .into_iter()
        .map(|id| SpaceSelector::Id(SpaceId::from(id)))
        .collect()
}

/// Spaces with agents or commands saved by `shoal stop` for `shoal
/// resume`; none when the daemon cannot say, which only loses the marks.
pub async fn stopped_spaces(paths: &CommandPaths) -> HashSet<SpaceId> {
    let summaries = saved_summaries(paths).await.into_values();
    let unfinished = summaries.filter(|summary| summary.unfinished);
    unfinished.map(|summary| summary.space).collect()
}
