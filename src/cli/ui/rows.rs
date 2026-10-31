//! The labels pickers and lists show: aligned rows of workspaces, agent
//! launches and repositories, with their status cells.
use std::{collections::HashSet, path::Path};

use super::Entries;

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        list_status::ListStatus,
        output::{Palette, Style},
    },
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

/// One aligned row per workspace: a status marker, the name, the repository
/// when the rows span several (`(id, name)` pairs), the status, what its
/// processes use, and the linked issue's title when known.
pub fn workspace_rows(
    workspaces: &[Workspace],
    repositories: &[(String, String)],
    stopped: &HashSet<String>,
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

/// `workspace_rows` for workspaces and the running chats that opened some
/// of them: the user's own workspaces, then each chat followed by its
/// workspaces, indented. Child workspaces follow their parent, indented
/// below it. A chat row names the chat, its status and its agent.
pub fn workspace_tree_rows(
    workspaces: &[Workspace],
    chats: &[Chat],
    stopped: &HashSet<String>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let tree = WorkspaceTree {
        workspaces,
        stopped,
        palette,
    };
    let opened_by = |w: &Workspace, chat: &Chat| w.chat.as_ref() == Some(&chat.id);
    let roots = tree.roots();
    let mut rows = Vec::new();
    for root in roots
        .iter()
        .filter(|w| !chats.iter().any(|chat| opened_by(w, chat)))
    {
        tree.push(&mut rows, root, 0);
    }
    for chat in chats {
        rows.push(chat_row(chat, palette));
        for root in roots.iter().filter(|w| opened_by(w, chat)) {
            tree.push(&mut rows, root, 1);
        }
    }
    let (markers, cells): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
    markers
        .into_iter()
        .zip(aligned_to(cells, palette, title_limit(width)))
        .map(|(marker, row)| format!("{marker} {row}"))
        .collect()
}

/// Listed workspaces arranged under their listed parents.
struct WorkspaceTree<'a> {
    workspaces: &'a [Workspace],
    stopped: &'a HashSet<String>,
    palette: Palette,
}

impl<'a> WorkspaceTree<'a> {
    /// The workspaces whose parent is not listed.
    fn roots(&self) -> Vec<&'a Workspace> {
        self.workspaces
            .iter()
            .filter(|w| {
                w.parent_workspace
                    .as_ref()
                    .is_none_or(|parent| !self.workspaces.iter().any(|p| p.id == parent.id))
            })
            .collect()
    }

    /// The row of `workspace` at `depth`, then those of its children below it.
    fn push(&self, rows: &mut Vec<(String, [Cell; 5])>, workspace: &Workspace, depth: usize) {
        rows.push(tree_workspace_row(
            workspace,
            depth,
            self.stopped,
            self.palette,
        ));
        for child in self.workspaces.iter().filter(|w| {
            w.parent_workspace
                .as_ref()
                .is_some_and(|parent| parent.id == workspace.id)
        }) {
            self.push(rows, child, depth + 1);
        }
    }
}

fn tree_workspace_row(
    workspace: &Workspace,
    depth: usize,
    stopped: &HashSet<String>,
    palette: Palette,
) -> (String, [Cell; 5]) {
    let name = match depth {
        0 => workspace.name.clone(),
        depth => format!("{}└ {}", "  ".repeat(depth - 1), workspace.name),
    };
    let status = ListStatus::of_workspace(workspace, stopped.contains(&workspace.id));
    (
        status.marker(palette),
        [
            (name, Some(Style::Heading)),
            (String::new(), None),
            workspace_status_cell(workspace, status),
            usage_cell(workspace),
            (
                workspace.links.issue_title.clone().unwrap_or_default(),
                None,
            ),
        ],
    )
}

/// Picker entries for running chats, labelled as lists show them.
pub fn chat_entries(chats: &[Chat], palette: Palette, width: Option<usize>) -> Entries {
    let (markers, cells): (Vec<_>, Vec<_>) =
        chats.iter().map(|chat| chat_row(chat, palette)).unzip();
    let labels = markers
        .into_iter()
        .zip(aligned_to(cells, palette, title_limit(width)))
        .map(|(marker, row)| format!("{marker} {row}"));
    chats
        .iter()
        .map(|chat| chat.id.clone())
        .zip(labels)
        .collect()
}

fn chat_row(chat: &Chat, palette: Palette) -> (String, [Cell; 5]) {
    let status = ListStatus::ProcessRunning;
    (
        palette.paint(Style::Heading, "◆"),
        [
            (chat.name.clone(), Some(Style::Heading)),
            (String::new(), None),
            (status.label().into(), Some(status.style())),
            (String::new(), None),
            (chat.agent.clone(), None),
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
    let cells: Vec<[Cell; 5]> = workspaces
        .iter()
        .zip(repository_column(workspaces, repositories))
        .zip(statuses)
        .map(|((w, repository), status)| {
            [
                (w.name.clone(), Some(Style::Heading)),
                (repository, None),
                status,
                usage_cell(w),
                (w.links.issue_title.clone().unwrap_or_default(), None),
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

/// Rows of `width` with a state marker before their cells cut the title.
fn title_limit(width: Option<usize>) -> Option<WidthLimit> {
    width.map(|width| WidthLimit {
        column: 4,
        width: width.saturating_sub(2),
    })
}

fn queue_status_cell(entry: &QueueEntry) -> Cell {
    let status = ListStatus::of_entry(entry);
    let mut text = match entry.position {
        Some(position) => format!("{}, position {position}", status.label()),
        None => status.label().into(),
    };
    if !entry.waiting_for.is_empty() {
        text += &format!(", after {}", entry.waiting_for.join(", "));
    }
    if entry.waits_for_update && entry.state == QueueState::Queued {
        text += ", after an update";
    }
    (text, Some(status.style()))
}

/// What a queue entry starts: a launch's issue title, or a restore.
fn queue_subject(entry: &QueueEntry) -> String {
    match entry.kind {
        QueueKind::Launch => entry.issue_title.clone().unwrap_or_default(),
        QueueKind::Restore => format!("Restore of {}", entry.agent),
    }
}

/// One aligned row per agent queue entry: its workspace name, its repository
/// when the entries span several, its place, and what it starts.
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
    let cells: Vec<[Cell; 4]> = entries
        .iter()
        .zip(names)
        .map(|(entry, repository)| {
            [
                (entry.name.clone(), Some(Style::Heading)),
                (
                    if several {
                        repository.into()
                    } else {
                        String::new()
                    },
                    None,
                ),
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
        workspaces.iter().map(|w| w.repository_id.as_str()),
        repositories,
    )
}

/// The name of each repository ID, or empty cells when all name one.
pub fn repository_cells<'a>(
    ids: impl Iterator<Item = &'a str>,
    repositories: &[(String, String)],
) -> Vec<String> {
    let names: Vec<&str> = ids
        .map(|repository| {
            repositories
                .iter()
                .find(|(id, _)| id == repository)
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

/// Picker entries that stay unambiguous when repositories share a name.
pub async fn repository_choices(mut repos: Vec<Repository>) -> Result<Entries> {
    for repo in &mut repos {
        if let Some(url) = crate::repository::remote_url_from_source(&repo.source).await? {
            repo.source = url;
        }
    }
    let labels: Vec<_> = repos
        .iter()
        .map(|repo| {
            let name = crate::repository::name(repo);
            let shared = repos
                .iter()
                .filter(|r| crate::repository::name(r) == name)
                .count()
                > 1;
            if !shared {
                return name.to_owned();
            }
            match crate::repository::host(&repo.source) {
                Some(host) => format!("{name} ({host})"),
                None => format!("{name} (local)"),
            }
        })
        .collect();
    Ok(repos
        .into_iter()
        .zip(&labels)
        .map(|(repo, label)| {
            // Same host or multiple local checkouts can still share a name.
            let label = if labels.iter().filter(|other| *other == label).count() > 1 {
                format!("{label}  {}", repo.source)
            } else {
                label.clone()
            };
            (repo.id, label)
        })
        .collect())
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
