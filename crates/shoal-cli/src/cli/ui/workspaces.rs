//! Choosing the workspace a command acts on: the named one, the current
//! one, or one from a picker.
use shoal_ids::space_ids::SpaceId;
use std::collections::HashSet;

use anyhow::{Result, ensure};

use super::{
    Acting, Entries, elsewhere_entries, pick, picker_width, require_argument, rows_with_status,
    spaces::stopped_spaces, status_cell,
};
use crate::{
    cli::{
        args::SpaceFilter,
        availability::{self, Availability, Fit},
        context::Context,
        list_status::ListStatus,
        output::{Palette, Style},
        recent,
    },
    model::Workspace,
    scoping::Scoping,
    workspace_context::{self, ScopeOrder, WorkspaceContext},
};
use shoal_protocol::hub::directory::Offered;
use shoal_protocol::space_names::SpaceSelector;

/// How to choose a workspace when the command line names none.
#[derive(Clone, Copy)]
pub enum Fallback<'a> {
    /// Use the worktree containing the current directory, else open the
    /// picker, which offers the spaces that what the command does with the
    /// picked one reaches.
    CurrentDirectory(&'a Acting),
    /// Use only the current workspace; never open a picker.
    CurrentDirectoryOnly,
    /// Use the space the caller's scope belongs to, a workspace or a chat,
    /// else the current workspace; never open a picker. Agent hooks report
    /// for the agent's own space wherever its shell has moved, such as into
    /// a child workspace.
    OwnSpace,
}

pub use crate::workspace_context::NoCurrentWorkspace;

pub(in crate::cli) use crate::fsutil::current_dir::current_directory;

pub async fn select_workspace(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback<'_>,
) -> Result<SpaceSelector> {
    let workspace = choose_workspace(ctx, explicit, fallback).await?;
    record_workspace(ctx, &workspace).await;
    Ok(workspace)
}

/// Remember the workspace a command that acts only on workspaces selected;
/// a chat named to it, which it refuses, stays out of its history.
pub(in crate::cli) async fn record_workspace(ctx: &Context, workspace: &SpaceSelector) {
    recent::record_where(ctx, workspace, |space| space.kind().has_worktree()).await;
}

/// The workspace `select_workspace` selects, left out of the command's
/// history for a command that records it once it has acted.
pub async fn choose_workspace(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback<'_>,
) -> Result<SpaceSelector> {
    Ok(match resolve_workspace(ctx, explicit, fallback).await? {
        Resolution::Named(selector) => selector,
        Resolution::Current(id) => SpaceSelector::Id(id),
        Resolution::Picker(workspaces, acting) => {
            SpaceSelector::Id(pick_workspace(ctx, workspaces, WORKSPACE, acting).await?)
        }
    })
}

/// The workspace an explicit argument names, where `-` is the previous one.
pub async fn explicit_workspace(ctx: &Context, selector: SpaceSelector) -> Result<SpaceSelector> {
    if recent::is_previous(&selector) {
        Ok(SpaceSelector::Id(recent::previous(ctx).await?))
    } else {
        Ok(selector)
    }
}

/// `Some(workspace)` unless `--all` was passed or picked; the picker offers
/// every workspace after the individual ones.
pub async fn select_workspace_filter(
    ctx: &Context,
    filter: SpaceFilter,
    acting: &Acting,
) -> Result<Option<SpaceSelector>> {
    if filter.all {
        return Ok(None);
    }
    Ok(
        match resolve_workspace(ctx, filter.space, Fallback::CurrentDirectory(acting)).await? {
            Resolution::Named(selector) => Some(selector),
            Resolution::Current(id) => Some(SpaceSelector::Id(id)),
            Resolution::Picker(workspaces, acting) => {
                (pick_workspace_or_all(ctx, workspaces, acting).await?).map(SpaceSelector::Id)
            }
        },
    )
}

async fn pick_workspace_or_all(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    acting: &Acting,
) -> Result<Option<SpaceId>> {
    let mut entries = workspace_entries(ctx, workspaces, true).await;
    entries.extend(elsewhere_entries(ctx, Offered::Workspaces, acting).await);
    entries.push((ALL_ID.to_owned(), ALL_WORKSPACES.to_owned()));
    require_argument(ctx, "workspace", WORKSPACE)?;
    let id = pick(ctx, "Workspace> ", entries)?;
    Ok((id != ALL_ID).then(|| SpaceId::from(id)))
}

/// How pickers and completion name `--all`.
pub const ALL_WORKSPACES: &str = "All workspaces";
/// The picker ID of the entry that stands for `--all`; no workspace has it.
pub(super) const ALL_ID: &str = "--all";

pub(super) enum Resolution<'a> {
    /// The space the command line names, or the previous one.
    Named(SpaceSelector),
    /// The current workspace.
    Current(SpaceId),
    /// No workspace is explicit or current; the user picks among these, for
    /// what the command does with the picked one.
    Picker(Vec<Workspace>, &'a Acting),
}

pub(super) async fn resolve_workspace<'a>(
    ctx: &Context,
    explicit: Option<SpaceSelector>,
    fallback: Fallback<'a>,
) -> Result<Resolution<'a>> {
    if let Some(selector) = explicit {
        return Ok(Resolution::Named(explicit_workspace(ctx, selector).await?));
    }
    // A scoped caller lists its own workspace first, then the workspaces
    // split from it, which the current directory selects.
    let workspaces = crate::client::spaces::list(&ctx.paths)
        .await?
        .into_workspaces();
    let order = match fallback {
        Fallback::OwnSpace => ScopeOrder::BeforeDirectory,
        Fallback::CurrentDirectory(_) | Fallback::CurrentDirectoryOnly => {
            ScopeOrder::AfterDirectory
        }
    };
    if let Some(workspace) = workspace_context::current(&workspaces, order)? {
        return Ok(Resolution::Current(workspace.id.clone()));
    }
    match fallback {
        Fallback::CurrentDirectory(acting) => Ok(Resolution::Picker(workspaces, acting)),
        Fallback::CurrentDirectoryOnly | Fallback::OwnSpace => Err(NoCurrentWorkspace.into()),
    }
}

/// The workspace containing the current directory, for an unscoped caller.
pub fn current_workspace(workspaces: &[Workspace]) -> Result<Option<&Workspace>> {
    if Scoping::of_workspace() == Scoping::Scoped {
        return Ok(None);
    }
    let cwd = current_directory()?;
    let context = WorkspaceContext::from_directory(workspaces, cwd.as_deref());
    Ok(context.resolve(None, Scoping::Unscoped, ScopeOrder::BeforeDirectory))
}

/// Always run the picker, including for scoped callers (whose list is filtered).
pub async fn workspace_picker(ctx: &Context, acting: &Acting) -> Result<SpaceId> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    pick_workspace(ctx, spaces.into_workspaces(), WORKSPACE, acting).await
}

/// The command's argument that names the workspace it acts on.
const WORKSPACE: &str = "workspace";

/// Pick one of `workspaces`, or for a command that acts on them one of the
/// other joined machines' workspaces after them, for the command's
/// `argument`, the ID or value name that a refusal without a terminal
/// names.
pub async fn pick_workspace(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    argument: &str,
    acting: &Acting,
) -> Result<SpaceId> {
    let mut entries = workspace_entries(ctx, workspaces, true).await;
    entries.extend(elsewhere_entries(ctx, Offered::Workspaces, acting).await);
    ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
    require_argument(ctx, "workspace", argument)?;
    Ok(SpaceId::from(pick(ctx, "Workspace> ", entries)?))
}

/// Pick one of `workspaces`, which a command chose from this machine's
/// records, such as the workspaces another can build on, for the command's
/// `argument`.
pub async fn pick_listed_workspace(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    argument: &str,
) -> Result<SpaceId> {
    let entries = workspace_entries(ctx, workspaces, true).await;
    ensure!(!entries.is_empty(), "{}", ctx.need.none_offered());
    require_argument(ctx, "workspace", argument)?;
    Ok(SpaceId::from(pick(ctx, "Workspace> ", entries)?))
}

/// The picker entries for `workspaces`, judged against what the command
/// needs, starting with their state markers when `markers` says.
pub(super) async fn workspace_entries(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    markers: bool,
) -> Entries {
    let availability = availability::load(ctx.need, &ctx.paths, &workspaces).await;
    let stopped = stopped_spaces(&ctx.paths).await;
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
    stopped: &HashSet<SpaceId>,
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
    workspaces
        .into_iter()
        .map(|w| w.id.into())
        .zip(labels)
        .collect()
}
