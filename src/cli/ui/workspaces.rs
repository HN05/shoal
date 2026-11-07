//! Choosing the workspace a command acts on: the named one, the current
//! one, or one from a picker.
use crate::cli::availability::NeedExt;
use crate::cli::list_status::ListStatusExt;
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::RecordExt;
use std::{collections::HashSet, io};

use anyhow::{Result, ensure};

use super::{
    Cell, Entries, chat_entries, pick, pick_many, picker_width, require_argument, rows_with_status,
    status_cell,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::WorkspaceScope,
        availability::{self, Availability, Fit},
        context::Context,
        list_status::ListStatus,
        output::{Palette, Style},
        recent,
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    execution::recovery,
    model::{Chat, Workspace},
    paths::Paths,
};

/// How to choose a workspace when the command line names none.
#[derive(Clone, Copy)]
pub enum Fallback {
    /// Use the worktree containing the current directory, else open the picker.
    CurrentDirectory,
    /// Use only the current workspace; never open a picker.
    CurrentDirectoryOnly,
    /// Use the workspace the caller's scope belongs to, else the current
    /// workspace; never open a picker. Agent hooks report for the agent's own
    /// workspace wherever its shell has moved, such as into a child workspace.
    OwnWorkspace,
}

#[derive(Debug)]
pub struct NoCurrentWorkspace;

impl std::fmt::Display for NoCurrentWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no current workspace; pass an explicit workspace")
    }
}

impl std::error::Error for NoCurrentWorkspace {}

pub(in crate::cli) fn current_directory() -> io::Result<Option<std::path::PathBuf>> {
    match std::env::current_dir().and_then(std::fs::canonicalize) {
        Ok(cwd) => Ok(Some(cwd)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub async fn select_workspace(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback,
) -> Result<String> {
    let workspace = choose_workspace(ctx, explicit, fallback).await?;
    recent::record(ctx, &workspace).await;
    Ok(workspace)
}

/// The workspace `explicit` names, else the current one, else one picked
/// among the ready workspaces, as a command that needs a ready workspace
/// offers them.
pub async fn select_ready_workspace(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
) -> Result<String> {
    let workspace = match resolve_workspace(ctx, explicit, Fallback::CurrentDirectory).await? {
        Resolution::Workspace(id) => id,
        Resolution::Picker(mut workspaces) => {
            workspaces.retain(|workspace| workspace.state == crate::state::WorkspaceState::Ready);
            pick_workspace(ctx, workspaces, WORKSPACE).await?
        }
    };
    recent::record(ctx, &workspace).await;
    Ok(workspace)
}

/// The workspace `select_workspace` selects, left out of the command's
/// history for a command that records it once it has acted.
pub async fn choose_workspace(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback,
) -> Result<String> {
    Ok(match resolve_workspace(ctx, explicit, fallback).await? {
        Resolution::Workspace(id) => id,
        Resolution::Picker(workspaces) => pick_workspace(ctx, workspaces, WORKSPACE).await?,
    })
}

/// The workspaces a command that acts on several takes: those named, each
/// once and in order, otherwise the current one, otherwise those picked.
pub async fn select_workspaces(ctx: &Context, names: Vec<SpaceSelector>) -> Result<Vec<String>> {
    let mut selected = named_workspaces(ctx, names).await?;
    if selected.is_empty() {
        selected = match resolve_workspace(ctx, None, Fallback::CurrentDirectory).await? {
            Resolution::Workspace(id) => vec![id],
            Resolution::Picker(workspaces) => pick_workspaces(ctx, workspaces).await?,
        };
    }
    for workspace in &selected {
        recent::record(ctx, workspace).await;
    }
    Ok(selected)
}

/// The spaces, workspaces or chats, a command that acts on several takes:
/// those named, each once and in order, otherwise the current workspace,
/// otherwise those picked among the workspaces and the running chats, or
/// every chat when `stopped_chats`. It ends what it acts on, so it never
/// defaults to the chat it runs in. A name no workspace has stays as given,
/// for a chat or the daemon's refusal.
pub async fn select_spaces(
    ctx: &Context,
    names: Vec<SpaceSelector>,
    stopped_chats: bool,
) -> Result<Vec<String>> {
    let mut selected = named_workspaces(ctx, names).await?;
    if selected.is_empty() {
        selected = match resolve_workspace(ctx, None, Fallback::CurrentDirectory).await? {
            Resolution::Workspace(id) => vec![id],
            Resolution::Picker(workspaces) => {
                let chats = match stopped_chats {
                    true => crate::client::chats::recorded(&ctx.paths).await?,
                    false => crate::client::chats::list(&ctx.paths).await?,
                };
                require_argument(ctx, "spaces", SPACE)?;
                let entries = space_entries(ctx, workspaces, &chats).await;
                ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
                pick_many(ctx, "Spaces> ", entries)?
            }
        };
    }
    for space in &selected {
        recent::record(ctx, space).await;
    }
    Ok(selected)
}

/// The workspace IDs `names` select, each once and in order, so a workspace
/// named both by its name and by its ID is acted on once. A name no
/// workspace has stays as given, for the daemon to refuse.
async fn named_workspaces(ctx: &Context, names: Vec<SpaceSelector>) -> Result<Vec<String>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let context = WorkspaceContext::from_directory(&workspaces, None);
    let mut selected = Vec::new();
    for name in names {
        let workspace = match recent::is_previous(&name) {
            true => recent::previous(ctx).await?,
            false => (context.resolve(Some(&name), false, ScopeOrder::AfterDirectory))
                .map_or_else(|| name.to_string(), |workspace| workspace.id.clone()),
        };
        if !selected.contains(&workspace) {
            selected.push(workspace);
        }
    }
    Ok(selected)
}

/// The workspace an explicit argument names, where `-` is the previous one.
pub async fn explicit_workspace(ctx: &Context, selector: SpaceSelector) -> Result<String> {
    if recent::is_previous(&selector) {
        recent::previous(ctx).await
    } else {
        Ok(selector.to_string())
    }
}

/// `Some(workspace)` unless `--all` was passed or picked; the picker offers
/// every workspace after the individual ones.
pub async fn select_workspace_filter(
    ctx: &Context,
    scope: WorkspaceScope,
) -> Result<Option<String>> {
    if scope.all {
        return Ok(None);
    }
    match resolve_workspace(ctx, scope.workspace, Fallback::CurrentDirectory).await? {
        Resolution::Workspace(id) => Ok(Some(id)),
        Resolution::Picker(workspaces) => pick_workspace_or_all(ctx, workspaces).await,
    }
}

/// Like `select_workspaces`, but the current workspace counts only with
/// something to resume, and the picker lists what each workspace has to
/// resume beside the `saved` chats. `None` when nothing is resumable
/// anywhere.
pub async fn select_stopped_spaces(
    ctx: &Context,
    names: Vec<SpaceSelector>,
    saved: &[Chat],
) -> Result<Option<Vec<String>>> {
    if !names.is_empty() {
        return select_workspaces(ctx, names).await.map(Some);
    }
    let workspaces = match resolve_workspace(ctx, None, Fallback::CurrentDirectory).await? {
        Resolution::Workspace(id) if resumable(&ctx.paths, &id).is_some() => {
            return Ok(Some(vec![id]));
        }
        Resolution::Workspace(_) => crate::client::workspaces::list(&ctx.paths).await?,
        Resolution::Picker(workspaces) => workspaces,
    };
    let Some(entries) = resume_entries(ctx, workspaces, saved) else {
        return Ok(None);
    };
    require_argument(ctx, "spaces", SPACE)?;
    pick_many(ctx, "Spaces> ", entries).map(Some)
}

/// Workspaces with stopped work come first, then those with a last session,
/// then the `saved` chats, then the workspaces with nothing to resume;
/// `None` when nothing is resumable.
fn resume_entries(ctx: &Context, workspaces: Vec<Workspace>, saved: &[Chat]) -> Option<Entries> {
    let mut rows = workspaces
        .into_iter()
        .map(|w| (resumable(&ctx.paths, &w.id), w))
        .collect::<Vec<_>>();
    if saved.is_empty() && rows.iter().all(|(resumable, _)| resumable.is_none()) {
        return None;
    }
    rows.sort_by_key(|(resumable, _)| match resumable {
        Some((_, true)) => 0,
        Some((_, false)) => 1,
        None => 2,
    });
    let with_work = rows
        .iter()
        .filter(|(resumable, _)| resumable.is_some())
        .count();
    let (statuses, workspaces): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|(resumable, w)| {
            let stopped = matches!(resumable, Some((_, true)));
            let status = ListStatus::of_workspace(&w, stopped);
            ((status, resume_status_cell(resumable)), w)
        })
        .unzip();
    let palette = Palette::stderr(ctx.json);
    let labels = rows_with_status(&workspaces, &[], statuses, palette, picker_width(), false);
    let mut entries: Entries = workspaces.into_iter().map(|w| w.id).zip(labels).collect();
    let chats = chat_entries(saved, palette, picker_width());
    entries.splice(with_work..with_work, chats);
    Some(entries)
}

/// What `shoal resume` restores in a workspace, such as `stopped: claude,
/// 1 command`, and whether it is unfinished work rather than only last
/// sessions; `None` when nothing is saved.
pub fn resumable(paths: &Paths, workspace_id: &str) -> Option<(String, bool)> {
    let saved = recovery::Saved::load(paths, workspace_id).unwrap_or_default();
    if saved.is_empty() {
        return None;
    }
    let records = saved
        .agents
        .iter()
        .map(|(_, path)| recovery::Record::load(path).map(|record| record.map(|r| r.agent)));
    let sessions = saved
        .sessions
        .iter()
        .map(|(_, path)| recovery::LastSession::load(path).map(|last| last.map(|l| l.agent)));
    let mut parts = records
        .chain(sessions)
        .map(|agent| agent.ok().flatten().unwrap_or_else(|| "agent".to_owned()))
        .collect::<Vec<_>>();
    match saved.commands.len() {
        0 => {}
        1 => parts.push("1 command".into()),
        n => parts.push(format!("{n} commands")),
    }
    Some((
        format!("stopped: {}", parts.join(", ")),
        saved.is_unfinished(),
    ))
}

/// How the resume picker and completion name a workspace without stopped work.
pub const NOTHING_TO_RESUME: &str = "nothing to resume";

fn resume_status_cell(resumable: Option<(String, bool)>) -> Cell {
    match resumable {
        Some((text, true)) => (text, Some(ListStatus::Stopped.style())),
        Some((text, false)) => (text, None),
        None => (NOTHING_TO_RESUME.into(), Some(Style::Muted)),
    }
}

async fn pick_workspace_or_all(
    ctx: &Context,
    workspaces: Vec<Workspace>,
) -> Result<Option<String>> {
    let mut entries = workspace_entries(ctx, workspaces, true).await;
    entries.push((ALL_ID.to_owned(), ALL_WORKSPACES.to_owned()));
    require_argument(ctx, "workspace", WORKSPACE)?;
    let id = pick(ctx, "Workspace> ", entries)?;
    Ok((id != ALL_ID).then_some(id))
}

/// How pickers and completion name `--all`.
pub const ALL_WORKSPACES: &str = "All workspaces";
/// The picker ID of the entry that stands for `--all`; no workspace has it.
const ALL_ID: &str = "--all";

enum Resolution {
    Workspace(String),
    /// No workspace is explicit or current; the user picks among these.
    Picker(Vec<Workspace>),
}

async fn resolve_workspace(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback,
) -> Result<Resolution> {
    if let Some(selector) = explicit {
        return Ok(Resolution::Workspace(
            explicit_workspace(ctx, selector).await?,
        ));
    }
    // A scoped caller lists its own workspace first, then the workspaces
    // split from it, which the current directory selects.
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let scoped = crate::env::workspace_scoped();
    let order = match fallback {
        Fallback::OwnWorkspace => ScopeOrder::BeforeDirectory,
        Fallback::CurrentDirectory | Fallback::CurrentDirectoryOnly => ScopeOrder::AfterDirectory,
    };
    let cwd = if order.reads_directory(scoped) {
        current_directory()?
    } else {
        None
    };
    let context = WorkspaceContext::from_directory(&workspaces, cwd.as_deref());
    if let Some(workspace) = context.resolve(None, scoped, order) {
        return Ok(Resolution::Workspace(workspace.id.clone()));
    }
    ensure!(!scoped, "scoped workspace is unavailable");
    if matches!(
        fallback,
        Fallback::CurrentDirectoryOnly | Fallback::OwnWorkspace
    ) {
        return Err(NoCurrentWorkspace.into());
    }
    Ok(Resolution::Picker(workspaces))
}

/// The workspace containing the current directory, for an unscoped caller.
pub fn current_workspace(workspaces: &[Workspace]) -> Result<Option<&Workspace>> {
    if crate::env::workspace_scoped() {
        return Ok(None);
    }
    let cwd = current_directory()?;
    let context = WorkspaceContext::from_directory(workspaces, cwd.as_deref());
    Ok(context.resolve(None, false, ScopeOrder::BeforeDirectory))
}

/// Always run the picker, including for scoped callers (whose list is filtered).
pub async fn workspace_picker(ctx: &Context) -> Result<String> {
    pick_workspace(
        ctx,
        crate::client::workspaces::list(&ctx.paths).await?,
        WORKSPACE,
    )
    .await
}

/// The command's argument that names the workspace it acts on.
const WORKSPACE: &str = "workspace";

/// Pick one of `workspaces` for the command's `argument`, the ID or value
/// name that a refusal without a terminal names.
pub async fn pick_workspace(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    argument: &str,
) -> Result<String> {
    let entries = workspace_entries(ctx, workspaces, true).await;
    ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
    require_argument(ctx, "workspace", argument)?;
    pick(ctx, "Workspace> ", entries)
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
    fallback: Fallback,
) -> Result<String> {
    match current_chat(&explicit) {
        Some(chat) if matches!(fallback, Fallback::OwnWorkspace) => Ok(chat),
        Some(chat) => current_workspace_or_chat(ctx, chat, Workspaces::Below).await,
        None => select_other_space(ctx, explicit, fallback).await,
    }
}

/// [`select_space`] for a command that shows where it runs: in a chat, any
/// workspace containing the current directory comes before the chat, so a
/// chat in a workspace's worktree, which reads that workspace without
/// changing it, gets the workspace.
pub async fn select_space_here(ctx: &Context, explicit: Option<SpaceSelector>) -> Result<String> {
    match current_chat(&explicit) {
        Some(chat) => current_workspace_or_chat(ctx, chat, Workspaces::Any).await,
        None => select_other_space(ctx, explicit, Fallback::CurrentDirectory).await,
    }
}

/// Which workspaces containing the current directory come before the chat.
enum Workspaces {
    /// Those below the chat, which its scope covers.
    Below,
    /// Any, such as the one whose worktree the chat runs in.
    Any,
}

/// The workspace of `which` containing the current directory, else `chat`.
async fn current_workspace_or_chat(
    ctx: &Context,
    chat: String,
    which: Workspaces,
) -> Result<String> {
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    let below = spaces.below(&chat);
    let current = current_workspace(&spaces.workspaces)?.filter(|workspace| match which {
        Workspaces::Below => below.iter().any(|space| space.id == workspace.id),
        Workspaces::Any => true,
    });
    let Some(workspace) = current else {
        return Ok(chat);
    };
    recent::record(ctx, &workspace.id).await;
    Ok(workspace.id.clone())
}

/// [`select_space`] for a command that never acts on the chat it runs in,
/// such as one that ends the space it acts on: without one `explicit`, the
/// workspace `fallback` chooses or a picker decides.
pub async fn select_other_space(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback,
) -> Result<String> {
    let space = match resolve_workspace(ctx, explicit, fallback).await? {
        Resolution::Workspace(id) => id,
        Resolution::Picker(workspaces) => {
            let chats = crate::client::chats::list(&ctx.paths).await?;
            pick_space(ctx, workspaces, &chats, SPACE).await?
        }
    };
    recent::record(ctx, &space).await;
    Ok(space)
}

/// The chat this process runs in, outside any workspace, when no space is
/// `explicit`.
pub fn current_chat(explicit: &Option<SpaceSelector>) -> Option<String> {
    if explicit.is_some() {
        return None;
    }
    crate::env::in_chat()
}

/// [`select_space`] for a command that also acts on every space: `None`
/// when `--all` was passed or picked.
pub async fn select_space_filter(ctx: &Context, scope: WorkspaceScope) -> Result<Option<String>> {
    if scope.all {
        return Ok(None);
    }
    match resolve_workspace(ctx, scope.workspace, Fallback::CurrentDirectory).await? {
        Resolution::Workspace(id) => Ok(Some(id)),
        Resolution::Picker(workspaces) => {
            let chats = crate::client::chats::list(&ctx.paths).await?;
            pick_space_or_all(ctx, workspaces, &chats).await
        }
    }
}

/// Pick a space as [`pick_space`] offers them, or every space after them:
/// `None` for that.
async fn pick_space_or_all(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    chats: &[Chat],
) -> Result<Option<String>> {
    let mut entries = space_entries(ctx, workspaces, chats).await;
    entries.push((ALL_ID.to_owned(), ALL_SPACES.to_owned()));
    require_argument(ctx, "space", SPACE)?;
    let id = pick(ctx, "Space> ", entries)?;
    Ok((id != ALL_ID).then_some(id))
}

/// The command's argument that names the space it acts on.
const SPACE: &str = "space";

/// How the space picker names `--all`.
const ALL_SPACES: &str = "All spaces";

/// Pick a space: a workspace, as [`pick_workspace`] offers them, or one of
/// `chats`, listed after them by name, for the command's `argument`.
/// Returns the chosen workspace's or chat's ID.
pub async fn pick_space(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    chats: &[Chat],
    argument: &str,
) -> Result<String> {
    let entries = space_entries(ctx, workspaces, chats).await;
    ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
    require_argument(ctx, "space", argument)?;
    pick(ctx, "Space> ", entries)
}

/// The picker entries for `workspaces`, then `chats`.
async fn space_entries(ctx: &Context, workspaces: Vec<Workspace>, chats: &[Chat]) -> Entries {
    let mut entries = workspace_entries(ctx, workspaces, true).await;
    entries.extend(chat_entries(
        chats,
        Palette::stderr(ctx.json),
        picker_width(),
    ));
    entries
}

async fn pick_workspaces(ctx: &Context, workspaces: Vec<Workspace>) -> Result<Vec<String>> {
    // `stop` takes spaces, of which it picks workspaces.
    let argument = match crate::cli::usage::of(&ctx.command, WORKSPACE) {
        Some(_) => WORKSPACE,
        None => SPACE,
    };
    require_argument(ctx, "workspaces", argument)?;
    let entries = workspace_entries(ctx, workspaces, false).await;
    ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
    pick_many(ctx, "Workspaces> ", entries)
}

/// The picker entries for `workspaces`, judged against what the command
/// needs, starting with their state markers when `markers` says.
async fn workspace_entries(ctx: &Context, workspaces: Vec<Workspace>, markers: bool) -> Entries {
    let availability = availability::load(ctx.need, &ctx.paths, &workspaces).await;
    let stopped = stopped_workspaces(&ctx.paths, &workspaces);
    availability_entries(
        workspaces,
        &availability,
        &stopped,
        Palette::stderr(ctx.json),
        markers,
    )
}

/// Workspaces the command would act on come first; those it would refuse
/// follow with the reason in place of their status, and those without what
/// it acts on are left out.
fn availability_entries(
    workspaces: Vec<Workspace>,
    availability: &Availability,
    stopped: &HashSet<String>,
    palette: Palette,
    markers: bool,
) -> Entries {
    let mut rows = workspaces
        .into_iter()
        .filter_map(|w| match availability.fit(&w) {
            Fit::Accepted => Some((None, w)),
            Fit::Refused(reason) => Some((Some(reason), w)),
            Fit::Hidden => None,
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|(refusal, _)| refusal.is_some());
    let (statuses, workspaces): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|(refusal, w)| {
            let status = ListStatus::of_workspace(&w, stopped.contains(&w.id));
            let cell = match refusal {
                Some(refusal) => (refusal, Some(Style::Muted)),
                None => status_cell(status),
            };
            ((status, cell), w)
        })
        .unzip();
    let labels = rows_with_status(&workspaces, &[], statuses, palette, picker_width(), markers);
    workspaces.into_iter().map(|w| w.id).zip(labels).collect()
}

/// Workspaces with agents or commands saved by `shoal stop` for `shoal resume`.
pub fn stopped_workspaces(paths: &Paths, workspaces: &[Workspace]) -> HashSet<String> {
    workspaces
        .iter()
        .filter(|w| recovery::pending(paths, &w.id).unwrap_or(false))
        .map(|w| w.id.clone())
        .collect()
}
