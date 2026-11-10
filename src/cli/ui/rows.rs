//! The labels pickers and lists show: aligned rows of workspaces, agent
//! launches and repositories, with their status cells.
use crate::cli::list_status::ListStatusExt;
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::{SpaceId, SpaceNumber};
use std::{collections::HashSet, path::Path, time::Duration};

use super::Entries;

use anyhow::{Context as _, Result};
use futures_util::future::join_all;

use crate::{
    cli::{
        list_status::ListStatus,
        output::{Palette, Style},
    },
    client::progress::Progress,
    config::modes::ModeName,
    forge::items::link,
    model::{Chat, QueueEntry, Repository, ReviewMark, Workspace, WorkspaceUsage},
    state::{AgentState, QueueKind, QueueState},
};

/// `items` joined by `separator` into lines no wider than `width`, where
/// one item fits; no limit keeps one line.
pub fn wrapped(items: &[String], separator: &str, width: Option<usize>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        match lines.last_mut() {
            Some(line)
                if width.is_none_or(|width| {
                    line.chars().count() + separator.len() + item.chars().count() <= width
                }) =>
            {
                line.push_str(separator);
                line.push_str(item);
            }
            _ => lines.push(item.clone()),
        }
    }
    lines.join("\n")
}

/// How lists and status name an agent's reported turn state.
pub fn agent_state_label(state: AgentState) -> &'static str {
    ListStatus::agent(state).label()
}

pub(super) fn status_cell(status: ListStatus) -> Cell {
    (status.label().into(), Some(status.style()))
}

/// The workspace's status followed by its latest review's, such as
/// `waiting, review 17 finished: 1 blocker`.
fn workspace_status_cell(workspace: &Workspace, status: ListStatus) -> Cell {
    let (label, style) = status_cell(status);
    match &workspace.latest_review {
        Some(review) => (
            format!("{label}, review {} {}", review.id, review.status),
            style,
        ),
        None => (label, style),
    }
}

fn usage_cell(workspace: &Workspace) -> Cell {
    (
        workspace
            .usage
            .as_ref()
            .map(usage_label)
            .unwrap_or_default(),
        Some(Style::Muted),
    )
}

/// A size in bytes as lists show it: `512 B`, `12 KiB`, `640 MiB` or `1.5 GiB`.
pub fn size_label(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < KIB * KIB {
        format!("{:.0} KiB", bytes / KIB)
    } else if bytes < KIB * KIB * KIB {
        format!("{:.0} MiB", bytes / KIB / KIB)
    } else {
        format!("{:.1} GiB", bytes / KIB / KIB / KIB)
    }
}

/// How lists and status show what a workspace's processes use:
/// `12% CPU, 640 MiB`, without CPU until the daemon has measured it.
pub fn usage_label(usage: &WorkspaceUsage) -> String {
    let memory = size_label(usage.memory_bytes);
    match usage.cpu_percent {
        Some(cpu) => format!("{cpu:.0}% CPU, {memory}"),
        None => memory,
    }
}

/// One aligned row per workspace: a status marker, the number, the name,
/// the repository when the rows span several (`(id, name)` pairs), the
/// mode, the status, what its processes use, its progress, and its agent's
/// status line, else the linked issue's title when known.
pub fn workspace_rows(
    workspaces: &[Workspace],
    repositories: &[(String, String)],
    stopped: &HashSet<SpaceId>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let statuses = workspaces
        .iter()
        .map(|w| {
            let status = ListStatus::of_workspace(w, stopped.contains(&w.id));
            (status, workspace_status_cell(w, status))
        })
        .collect();
    rows_with_status(workspaces, repositories, statuses, palette, width, true)
}

/// `workspace_rows` for workspaces and the running and stopped chats in one
/// tree: the spaces whose parent is not listed, workspaces first, each
/// followed by the spaces below it, indented. A chat row names the chat, its
/// status and its agent.
pub fn workspace_tree_rows(
    workspaces: &[Workspace],
    chats: &[Chat],
    stopped: &HashSet<SpaceId>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let tree = SpaceTree {
        workspaces,
        chats,
        stopped,
        palette,
    };
    let mut rows = Vec::new();
    for root in tree.roots() {
        tree.push(&mut rows, root, 0);
    }
    let (markers, cells): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
    markers
        .into_iter()
        .zip(aligned_to(cells, palette, title_limit(width)))
        .map(|(marker, row)| format!("{marker} {row}"))
        .collect()
}

/// A listed space.
#[derive(Clone, Copy)]
enum Listed<'a> {
    Workspace(&'a Workspace),
    Chat(&'a Chat),
}

impl<'a> Listed<'a> {
    fn id(&self) -> &'a SpaceId {
        match self {
            Self::Workspace(workspace) => &workspace.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    /// The space it lists under: its parent, or for a CI job's workspace,
    /// which has none, the workspace its run's PR or branch links.
    fn parent(&self) -> Option<&'a SpaceId> {
        let parent = match self {
            Self::Workspace(workspace) => workspace.parent.as_ref(),
            Self::Chat(chat) => chat.parent.as_ref(),
        };
        let linked = match self {
            Self::Workspace(workspace) => workspace
                .job
                .as_ref()
                .and_then(|job| job.workspace_id.as_ref()),
            Self::Chat(_) => None,
        };
        parent.map(|parent| &parent.id).or(linked)
    }
}

/// Listed spaces arranged under their listed parents.
struct SpaceTree<'a> {
    workspaces: &'a [Workspace],
    chats: &'a [Chat],
    stopped: &'a HashSet<SpaceId>,
    palette: Palette,
}

impl<'a> SpaceTree<'a> {
    /// Every listed space, workspaces first.
    fn spaces(&self) -> impl Iterator<Item = Listed<'a>> + use<'a> {
        let workspaces = self.workspaces.iter().map(Listed::Workspace);
        workspaces.chain(self.chats.iter().map(Listed::Chat))
    }

    /// The spaces whose parent is not listed.
    fn roots(&self) -> Vec<Listed<'a>> {
        let listed: HashSet<&SpaceId> = self.spaces().map(|space| space.id()).collect();
        (self.spaces())
            .filter(|space| space.parent().is_none_or(|parent| !listed.contains(parent)))
            .collect()
    }

    /// The row of `space` at `depth`, then those of the spaces below it.
    fn push(&self, rows: &mut Vec<(String, [Cell; 8])>, space: Listed<'a>, depth: usize) {
        rows.push(match space {
            Listed::Workspace(workspace) => {
                tree_workspace_row(workspace, depth, self.stopped, self.palette)
            }
            Listed::Chat(chat) => chat_row(chat, depth, self.palette),
        });
        for child in self
            .spaces()
            .filter(|child| child.parent() == Some(space.id()))
        {
            self.push(rows, child, depth + 1);
        }
    }
}

/// `name` at `depth` in a tree, below its parent's row.
fn indented(name: &str, depth: usize) -> String {
    match depth {
        0 => name.to_owned(),
        depth => format!("{}└ {name}", "  ".repeat(depth - 1)),
    }
}

fn tree_workspace_row(
    workspace: &Workspace,
    depth: usize,
    stopped: &HashSet<SpaceId>,
    palette: Palette,
) -> (String, [Cell; 8]) {
    let name = indented(&workspace.name, depth);
    let status = ListStatus::of_workspace(workspace, stopped.contains(&workspace.id));
    (
        status.marker(palette),
        [
            number_cell(workspace.number),
            (name, Some(Style::Heading)),
            (String::new(), None),
            workspace_mode_cell(workspace),
            workspace_status_cell(workspace, status),
            usage_cell(workspace),
            progress_cell(workspace.progress.as_ref()),
            (workspace_text(workspace), None),
        ],
    )
}

/// What a workspace row ends with: its agent's status line, else what it
/// names as its work.
fn workspace_text(workspace: &Workspace) -> String {
    match status_line(workspace.progress.as_ref()) {
        Some(status) => status.to_owned(),
        None => workspace_title(workspace),
    }
}

fn status_line(progress: Option<&Progress>) -> Option<&str> {
    progress?.status.as_deref()
}

/// The most characters a progress cell shows of the step under way.
const STEP_WIDTH: usize = 32;

/// How far along a space's agent is, as `3/5 Fix the timeout`: the done
/// steps out of all, and the step under way.
fn progress_cell(progress: Option<&Progress>) -> Cell {
    let Some(progress) = progress.filter(|progress| !progress.steps.is_empty()) else {
        return (String::new(), None);
    };
    let count = format!("{}/{}", progress.done(), progress.steps.len());
    let text = match progress.current_step() {
        Some(step) => format!("{count} {}", truncated(step.name.clone(), STEP_WIDTH)),
        None => count,
    };
    (text, None)
}

/// What a workspace row names as its work: the CI job it runs, or the
/// linked issue's title when known.
fn workspace_title(workspace: &Workspace) -> String {
    match &workspace.job {
        Some(job) => job_subject(job),
        None => workspace.links.issue_title.clone().unwrap_or_default(),
    }
}

/// Picker entries for running chats, labelled as lists show them.
pub fn chat_entries(chats: &[Chat], palette: Palette, width: Option<usize>) -> Entries {
    let (markers, cells): (Vec<_>, Vec<_>) =
        chats.iter().map(|chat| chat_row(chat, 0, palette)).unzip();
    let labels = markers
        .into_iter()
        .zip(aligned_to(cells, palette, title_limit(width)))
        .map(|(marker, row)| format!("{marker} {row}"));
    chats
        .iter()
        .map(|chat| chat.id.to_string())
        .zip(labels)
        .collect()
}

/// A chat's row: its status marker, number and name, the workspace it
/// runs in, its mode, its status, its progress, and its agent's status
/// line, else its agent.
fn chat_row(chat: &Chat, depth: usize, palette: Palette) -> (String, [Cell; 8]) {
    let status = ListStatus::of_chat(chat);
    let place = chat.workspace.as_ref();
    (
        status.marker(palette),
        [
            number_cell(chat.number),
            (indented(&chat.name, depth), Some(Style::Heading)),
            (
                place.map_or_else(String::new, |workspace| format!("in {}", workspace.name)),
                None,
            ),
            chat_mode_cell(chat),
            status_cell(status),
            (String::new(), None),
            progress_cell(chat.progress.as_ref()),
            (
                status_line(chat.progress.as_ref())
                    .unwrap_or(chat.agent.name())
                    .to_owned(),
                None,
            ),
        ],
    )
}

/// `workspace_rows` with a status, which picks the marker, and a status cell
/// per workspace.
/// `with_markers` starts each row with its state marker, which multi-select
/// pickers leave out beside their selection marks.
pub(super) fn rows_with_status(
    workspaces: &[Workspace],
    repositories: &[(String, String)],
    statuses: Vec<(ListStatus, Cell)>,
    palette: Palette,
    width: Option<usize>,
    with_markers: bool,
) -> Vec<String> {
    let (markers, statuses): (Vec<_>, Vec<_>) = statuses
        .into_iter()
        .map(|(status, cell)| (status.marker(palette), cell))
        .unzip();
    let cells: Vec<[Cell; 8]> = workspaces
        .iter()
        .zip(repository_column(workspaces, repositories))
        .zip(statuses)
        .map(|((w, repository), status)| {
            [
                number_cell(w.number),
                (w.name.to_string(), Some(Style::Heading)),
                (repository, None),
                workspace_mode_cell(w),
                status,
                usage_cell(w),
                progress_cell(w.progress.as_ref()),
                (workspace_text(w), None),
            ]
        })
        .collect();
    markers
        .into_iter()
        .zip(aligned_to(cells, palette, title_limit(width)))
        .map(|(marker, row)| {
            if with_markers {
                format!("{marker} {row}")
            } else {
                row
            }
        })
        .collect()
}

/// A space's `mode`, which rows leave out when it is the kind's `usual`
/// mode or none.
pub fn mode_cell(mode: Option<&ModeName>, usual: &ModeName) -> Cell {
    let shown = mode.filter(|mode| *mode != usual);
    (shown.map(ToString::to_string).unwrap_or_default(), None)
}

/// A workspace's mode, left out for `worker`.
pub fn workspace_mode_cell(workspace: &Workspace) -> Cell {
    mode_cell(workspace.mode.as_ref(), &ModeName::DEFAULT)
}

/// A chat's mode, left out for `chat`.
pub fn chat_mode_cell(chat: &Chat) -> Cell {
    mode_cell(Some(&chat.mode), &crate::model::chat_mode())
}

/// A space's number, as the first cell of its row.
fn number_cell(number: SpaceNumber) -> Cell {
    (number.to_string(), Some(Style::Muted))
}

/// Rows of `width` with a state marker before their cells cut the title.
fn title_limit(width: Option<usize>) -> Option<WidthLimit> {
    width.map(|width| WidthLimit {
        column: 7,
        width: width.saturating_sub(2),
    })
}

fn queue_status_cell(entry: &QueueEntry) -> Cell {
    let status = ListStatus::of_entry(entry);
    let label = status.label().to_owned();
    let mut text = match entry.position {
        Some(position) => format!("{label}, position {position}"),
        None => label,
    };
    if !entry.waiting_for.is_empty() {
        text += &format!(", after {}", entry.waiting_for.join(", "));
    }
    if entry.waits_for_update && entry.state == QueueState::Queued {
        text += ", after an update";
    }
    (text, Some(status.style()))
}

/// What a queue entry starts: a launch's issue title, a restore, or a CI job.
fn queue_subject(entry: &QueueEntry) -> String {
    match (entry.kind, &entry.job) {
        (QueueKind::Launch, _) => entry.issue_title.clone().unwrap_or_default(),
        (QueueKind::Restore, _) => format!("Restore of {}", entry.agent),
        (QueueKind::Job, Some(job)) => job_subject(job),
        (QueueKind::Job, None) => "CI job".into(),
    }
}

/// `CI job ci.yml / rust for PR #12`, naming the workflow where the forge
/// reports it and the PR or branch the run is for.
pub fn job_subject(queued: &crate::model::QueuedJob) -> String {
    use crate::forge::ci::runner::Trigger;
    let mut subject = match &queued.run.workflow {
        Some(workflow) => format!("CI job {workflow} / {}", queued.job.name),
        None => format!("CI job {}", queued.job.name),
    };
    match &queued.run.trigger {
        Trigger::Pull { number } => subject += &format!(" for PR #{number}"),
        Trigger::Branch(branch) => subject += &format!(" on {branch}"),
        Trigger::Other => {}
    }
    subject
}

/// One aligned row per agent queue entry: its workspace name, its repository
/// when the entries span several, its mode, its place, and what it starts.
pub fn queue_rows(
    entries: &[QueueEntry],
    repositories: &[Repository],
    palette: Palette,
) -> Vec<String> {
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| {
            repositories
                .iter()
                .find(|repo| repo.id == entry.repository_id)
                .map_or("unknown repository", crate::repository::name)
        })
        .collect();
    let several = names.iter().collect::<HashSet<_>>().len() > 1;
    let cells: Vec<[Cell; 5]> = entries
        .iter()
        .zip(names)
        .map(|(entry, repository)| {
            [
                (entry.name.to_string(), Some(Style::Heading)),
                (
                    if several {
                        repository.into()
                    } else {
                        String::new()
                    },
                    None,
                ),
                mode_cell(entry.mode.as_ref(), &ModeName::DEFAULT),
                queue_status_cell(entry),
                (queue_subject(entry), None),
            ]
        })
        .collect();
    entries
        .iter()
        .zip(aligned(cells, palette))
        .map(|(entry, row)| format!("{} {row}", ListStatus::of_entry(entry).marker(palette)))
        .collect()
}

/// `PR #12` or `issue #3` for a linked item, `workspace` for a mark made
/// while nothing was linked.
pub fn mark_label(mark: &ReviewMark) -> String {
    let (Some(kind), Some(url)) = (mark.kind, &mark.url) else {
        return "workspace".into();
    };
    let name = kind.noun();
    match link::item(kind.into(), url) {
        Ok((_, number)) => format!("{name} #{number}"),
        Err(_) => name.into(),
    }
}

/// Each workspace's repository name, or empty cells when all share one.
/// Repositories are `(id, name)` pairs.
fn repository_column(workspaces: &[Workspace], repositories: &[(String, String)]) -> Vec<String> {
    repository_cells(
        workspaces.iter().map(|w| Some(&w.repository_id)),
        repositories,
    )
}

/// The name of each repository ID, or empty cells when all name one.
pub fn repository_cells<'a>(
    ids: impl Iterator<Item = Option<&'a RepositoryId>>,
    repositories: &[(String, String)],
) -> Vec<String> {
    let names: Vec<&str> = ids
        .map(|repository| {
            repository
                .and_then(|repository| repositories.iter().find(|(id, _)| repository == id))
                .map_or("unknown repository", |(_, name)| name.as_str())
        })
        .collect();
    let several = names.iter().collect::<HashSet<_>>().len() > 1;
    names
        .into_iter()
        .map(|name| if several { name.into() } else { String::new() })
        .collect()
}

/// A table cell's text and its style.
pub type Cell = (String, Option<Style>);

/// Rows padded into columns two spaces apart. Columns empty in every row are
/// dropped, and each row ends at its last non-empty cell.
pub fn aligned<const N: usize>(rows: Vec<[Cell; N]>, palette: Palette) -> Vec<String> {
    aligned_to(rows, palette, None)
}

/// The rows' width limit, met by cutting the cells of one column, usually
/// free text, so the columns after it stay aligned and visible.
#[derive(Clone, Copy)]
pub struct WidthLimit {
    pub column: usize,
    pub width: usize,
}

/// A fitted column keeps this many characters however narrow the limit.
const MIN_FITTED: usize = 12;

/// `aligned`, cutting `limit`'s column with an ellipsis where rows would
/// exceed its width.
pub fn aligned_to<const N: usize>(
    rows: Vec<[Cell; N]>,
    palette: Palette,
    limit: Option<WidthLimit>,
) -> Vec<String> {
    let mut widths: Vec<usize> = (0..N)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].0.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    if let Some(limit) = limit {
        narrow(&mut widths, limit);
    }
    let rows = rows.into_iter().map(|row| {
        let mut column = 0;
        row.map(|(text, style)| {
            let cell = (truncated(text, widths[column]), style);
            column += 1;
            cell
        })
    });
    rows.map(|row| {
        let last = row.iter().rposition(|(text, _)| !text.is_empty());
        let mut line = String::new();
        let mut first = true;
        for (column, (text, style)) in row.into_iter().enumerate() {
            if widths[column] == 0 || Some(column) > last {
                continue;
            }
            if !std::mem::take(&mut first) {
                line.push_str("  ");
            }
            let padding = if Some(column) == last {
                0
            } else {
                widths[column] - text.chars().count()
            };
            line.push_str(&match style {
                Some(style) if !text.is_empty() => palette.paint(style, text),
                _ => text,
            });
            line.push_str(&" ".repeat(padding));
        }
        line
    })
    .collect()
}

/// Narrow `limit.column` by what the row width exceeds `limit.width` by.
fn narrow(widths: &mut [usize], limit: WidthLimit) {
    let shown = widths.iter().filter(|width| **width > 0).count();
    let total = widths.iter().sum::<usize>() + 2 * shown.saturating_sub(1);
    let Some(excess) = total.checked_sub(limit.width) else {
        return;
    };
    let column = &mut widths[limit.column];
    *column = column.saturating_sub(excess).max(MIN_FITTED.min(*column));
}

/// `text` cut to `width` characters, ending in an ellipsis when cut.
fn truncated(text: String, width: usize) -> String {
    if text.chars().count() <= width {
        return text;
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

pub fn repository_label(repo: &Repository, palette: Palette) -> String {
    format!(
        "{}  {}",
        palette.paint(Style::Heading, crate::repository::name(repo)),
        palette.paint(Style::Muted, &repo.source)
    )
}

/// How long a picker waits on a checkout's origin, as on a volume that stopped
/// answering, before labelling the repository by its recorded source.
const REMOTE_READ_LIMIT: Duration = Duration::from_secs(3);

/// Picker entries that stay unambiguous when repositories share a name.
pub async fn repository_choices(repos: Vec<Repository>) -> Entries {
    repository_entries(repos, crate::repository::remote_url_from_path).await
}

/// [`repository_choices`] with `read` reading a checkout's origin URL.
async fn repository_entries(
    repos: Vec<Repository>,
    read: impl AsyncFn(&Path) -> Result<Option<String>>,
) -> Entries {
    let shared = shared_names(&repos);
    let sources = distinguishing_sources(&repos, &shared, read).await;
    let labels = distinct_labels(&repos, &sources, &shared);
    repos
        .into_iter()
        .map(|repo| repo.id.into())
        .zip(labels)
        .collect()
}

/// Each repository's [`name_label`], followed by its source when that still
/// leaves two alike, as for checkouts from the same host.
fn distinct_labels(
    repos: &[Repository],
    sources: &[String],
    shared: &HashSet<&str>,
) -> Vec<String> {
    let labels: Vec<_> = repos
        .iter()
        .zip(sources)
        .map(|(repo, source)| name_label(repo, source, shared))
        .collect();
    labels
        .iter()
        .zip(sources)
        .map(|(label, source)| {
            if labels.iter().filter(|other| *other == label).count() > 1 {
                format!("{label}  {source}")
            } else {
                label.clone()
            }
        })
        .collect()
}

/// The repository's name, with the host of `source`, or `local`, when its
/// name is `shared`.
fn name_label(repo: &Repository, source: &str, shared: &HashSet<&str>) -> String {
    let name = crate::repository::name(repo);
    if !shared.contains(name) {
        return name.to_owned();
    }
    match crate::repository::host(source) {
        Some(host) => format!("{name} ({host})"),
        None => format!("{name} (local)"),
    }
}

/// The names more than one of `repos` has.
fn shared_names(repos: &[Repository]) -> HashSet<&str> {
    let mut seen = HashSet::new();
    repos
        .iter()
        .map(crate::repository::name)
        .filter(|name| !seen.insert(*name))
        .collect()
}

/// Each repository's source, with a checkout whose name is `shared` given
/// its origin's URL to tell it apart by host. Only those checkouts start
/// Git, all at once, and one whose origin `read` cannot give within
/// [`REMOTE_READ_LIMIT`] keeps its recorded source.
async fn distinguishing_sources(
    repos: &[Repository],
    shared: &HashSet<&str>,
    read: impl AsyncFn(&Path) -> Result<Option<String>>,
) -> Vec<String> {
    let read = &read;
    join_all(repos.iter().map(|repo| async move {
        let checkout = shared.contains(crate::repository::name(repo))
            && crate::repository::host(&repo.source).is_none();
        if !checkout {
            return repo.source.clone();
        }
        match tokio::time::timeout(REMOTE_READ_LIMIT, read(Path::new(&repo.source))).await {
            Ok(Ok(Some(url))) => url,
            _ => repo.source.clone(),
        }
    }))
    .await
}

/// Preserve the argument and resolve existing paths in the caller's directory.
pub fn repository_selector(value: String) -> Result<crate::repository::Selector> {
    if Path::new(&value).exists() {
        let path = std::fs::canonicalize(&value)?;
        path.to_str().context("repository path is not UTF-8")?;
        Ok(crate::repository::Selector::Path { value, path })
    } else {
        Ok(value.into())
    }
}

#[cfg(test)]
mod tests;
