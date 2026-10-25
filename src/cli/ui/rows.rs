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
    forge::items::link::{self, ItemKind},
    model::{Chat, QueueEntry, Repository, ReviewMark, Workspace},
    state::{AgentState, QueueKind},
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

/// One aligned row per workspace: a status marker, the name, the repository
/// when the rows span several (`(id, name)` pairs), the status, and the linked
/// issue's title when known.
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
            (status, status_cell(status))
        })
        .collect();
    rows_with_status(workspaces, repositories, statuses, palette, width)
}

/// `workspace_rows` for workspaces and the running chats that opened some
/// of them: the user's own workspaces, then each chat followed by its
/// workspaces, indented. Child workspaces follow their parent, indented
/// below it. A chat row names the chat, its status and its agent.
pub fn workspace_tree_rows(
    workspaces: &[Workspace],
    chats: &[Chat],
    repositories: &[Repository],
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
        rows.push(chat_row(chat, repositories, palette));
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
    fn push(&self, rows: &mut Vec<(String, [Cell; 4])>, workspace: &Workspace, depth: usize) {
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
) -> (String, [Cell; 4]) {
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
            status_cell(status),
            (
                workspace.links.issue_title.clone().unwrap_or_default(),
                None,
            ),
        ],
    )
}

fn chat_row(chat: &Chat, repositories: &[Repository], palette: Palette) -> (String, [Cell; 4]) {
    let status = ListStatus::ProcessRunning;
    (
        palette.paint(Style::Heading, "◆"),
        [
            (chat_label(chat, repositories), Some(Style::Heading)),
            (String::new(), None),
            (status.label().into(), Some(status.style())),
            (chat.agent.clone(), None),
        ],
    )
}

/// How lists name a chat: after its repository, numbered after the first.
pub fn chat_label(chat: &Chat, repositories: &[Repository]) -> String {
    let repository = repositories
        .iter()
        .find(|repo| repo.id == chat.repository_id)
        .map_or("unknown repository", crate::repository::name);
    match chat.number {
        1 => format!("{repository} chat"),
        number => format!("{repository} chat {number}"),
    }
}

/// `workspace_rows` with a status, which picks the marker, and a status cell
/// per workspace.
pub(super) fn rows_with_status(
    workspaces: &[Workspace],
    repositories: &[(String, String)],
    statuses: Vec<(ListStatus, Cell)>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let (markers, statuses): (Vec<_>, Vec<_>) = statuses
        .into_iter()
        .map(|(status, cell)| (status.marker(palette), cell))
        .unzip();
    let cells: Vec<[Cell; 4]> = workspaces
        .iter()
        .zip(repository_column(workspaces, repositories))
        .zip(statuses)
        .map(|((w, repository), status)| {
            [
                (w.name.clone(), Some(Style::Heading)),
                (repository, None),
                status,
                (w.links.issue_title.clone().unwrap_or_default(), None),
            ]
        })
        .collect();
    markers
        .into_iter()
        .zip(aligned_to(cells, palette, title_limit(width)))
        .map(|(marker, row)| format!("{marker} {row}"))
        .collect()
}

/// Rows of `width` with a state marker before their cells cut the title.
fn title_limit(width: Option<usize>) -> Option<WidthLimit> {
    width.map(|width| WidthLimit {
        column: 3,
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
    (text, Some(status.style()))
}

/// What a queue entry starts: a launch's issue title, or a restore.
fn queue_subject(entry: &QueueEntry) -> String {
    match entry.kind {
        QueueKind::Launch => entry.issue_title.clone().unwrap_or_default(),
        QueueKind::Restore => format!("Automatic restore of {}", entry.agent),
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
    let name = match kind {
        ItemKind::Pr => "PR",
        ItemKind::Issue => "issue",
    };
    match link::item(kind, url) {
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
mod tests {
    use super::*;
    use crate::state::WorkspaceState;

    fn workspace(repository: &str, name: &str, branch: &str, state: WorkspaceState) -> Workspace {
        Workspace::new_record(
            repository.into(),
            name.into(),
            format!("/work/{name}").into(),
            branch.into(),
            state,
        )
    }

    #[test]
    fn queue_status_names_the_work_a_launch_waits_for() {
        let mut entry = QueueEntry {
            id: "entry".into(),
            kind: QueueKind::Launch,
            repository_id: "repo".into(),
            name: "ui".into(),
            agent: "claude".into(),
            issue_title: None,
            state: crate::state::QueueState::Queued,
            position: Some(2),
            created_at: 0,
            workspace_id: None,
            opens: None,
            waiting_for: Vec::new(),
            base: None,
        };
        assert_eq!(queue_status_cell(&entry).0, "queued, position 2");
        entry.waiting_for = vec!["api".into(), "https://forge.example/o/r/issues/7".into()];
        assert_eq!(
            queue_status_cell(&entry).0,
            "queued, position 2, after api, https://forge.example/o/r/issues/7"
        );
    }

    #[test]
    fn tree_rows_list_children_below_their_parent() {
        let plain = Palette::stdout(true);
        let mut workspaces = vec![
            workspace("a", "child", "child", WorkspaceState::Ready),
            workspace("a", "grandchild", "grandchild", WorkspaceState::Ready),
            workspace("a", "orphan", "orphan", WorkspaceState::Ready),
            workspace("a", "parent", "parent", WorkspaceState::Ready),
        ];
        let reference = |w: &Workspace| crate::model::WorkspaceRef {
            id: w.id.clone(),
            name: w.name.clone(),
            branch: w.branch.clone(),
        };
        workspaces[0].parent_workspace = Some(reference(&workspaces[3]));
        workspaces[1].parent_workspace = Some(reference(&workspaces[0]));
        // A parent left out of the list leaves its child at the top level.
        workspaces[2].parent_workspace = Some(crate::model::WorkspaceRef {
            id: "unlisted".into(),
            name: "unlisted".into(),
            branch: "unlisted".into(),
        });
        let rows = workspace_tree_rows(&workspaces, &[], &[], &HashSet::new(), plain, None);
        let expected = ["○ orphan ", "○ parent ", "○ └ child ", "○   └ grandchild "];
        assert_eq!(rows.len(), expected.len());
        for (row, start) in rows.iter().zip(expected) {
            assert!(row.starts_with(start), "{row:?}");
        }
    }

    #[test]
    fn width_limits_cut_one_column_and_keep_the_rest_aligned() {
        let plain = Palette::stdout(true);
        let cell = |text: &str| (text.to_owned(), None);
        let rows = || {
            vec![
                [
                    cell("question"),
                    cell("Which database should the cache use?"),
                    cell("SQLite"),
                ],
                [cell("ready"), cell("PR #12"), cell("")],
            ]
        };
        let limit = |width| Some(WidthLimit { column: 1, width });
        assert_eq!(
            aligned_to(rows(), plain, limit(60)),
            aligned(rows(), plain),
            "rows that fit stay whole"
        );
        assert_eq!(
            aligned_to(rows(), plain, limit(40)),
            [
                "question  Which database should…  SQLite",
                "ready     PR #12",
            ]
        );
        // However narrow the limit, the column keeps a few characters.
        assert_eq!(
            aligned_to(rows(), plain, limit(10))[0],
            "question  Which datab…  SQLite"
        );
    }

    #[test]
    fn wrapped_items_fill_lines_up_to_the_width() {
        let items: Vec<String> = ["enter: cd", "tab: next view", "ctrl-d: delete"]
            .map(String::from)
            .into();
        assert_eq!(
            wrapped(&items, "   ", None),
            "enter: cd   tab: next view   ctrl-d: delete"
        );
        assert_eq!(
            wrapped(&items, "   ", Some(26)),
            "enter: cd   tab: next view\nctrl-d: delete"
        );
        // An item wider than the limit gets a line of its own.
        assert_eq!(
            wrapped(&items, "   ", Some(5)),
            "enter: cd\ntab: next view\nctrl-d: delete"
        );
    }

    #[test]
    fn mark_labels_name_the_marked_item() {
        let mark = |kind, url: Option<&str>| ReviewMark {
            kind,
            url: url.map(Into::into),
            head: "abc".into(),
            created_at: 0,
            stale: None,
            warnings: Vec::new(),
        };
        let pr = mark(Some(ItemKind::Pr), Some("https://example.com/o/r/pulls/12"));
        let issue = mark(
            Some(ItemKind::Issue),
            Some("https://example.com/o/r/issues/3"),
        );
        assert_eq!(mark_label(&pr), "PR #12");
        assert_eq!(mark_label(&issue), "issue #3");
        assert_eq!(mark_label(&mark(None, None)), "workspace");
    }

    #[test]
    fn workspace_rows_align_status_and_titles() {
        let plain = Palette::stdout(true);
        let mut workspaces = vec![
            workspace("a", "fix-login", "fix-login", WorkspaceState::Ready),
            workspace("a", "x", "feature/x", WorkspaceState::Failed),
            workspace("a", "new", "new", WorkspaceState::Preparing),
            workspace("a", "busy", "busy", WorkspaceState::Ready),
            workspace("a", "review", "review", WorkspaceState::Ready),
            workspace("a", "quiet", "quiet", WorkspaceState::Ready),
        ];
        workspaces[3].running = true;
        workspaces[3].links.issue_title = Some("Refine the list view".into());
        let mark = |stale| crate::model::ReviewMark {
            kind: None,
            url: None,
            head: "head".into(),
            created_at: 0,
            stale: Some(stale),
            warnings: Vec::new(),
        };
        workspaces[4].running = true;
        workspaces[4].review = vec![mark(false)];
        workspaces[5].review = vec![mark(true)];
        let agent = |state| Some(crate::model::AgentStatus { state, since: 0 });
        // Needing input outranks completion, which outranks a current mark
        // and other turn states.
        workspaces.extend([
            workspace("a", "asking", "asking", WorkspaceState::Ready),
            workspace("a", "marked", "marked", WorkspaceState::Ready),
            workspace("a", "agent", "agent", WorkspaceState::Ready),
            workspace("a", "done", "done", WorkspaceState::Ready),
            workspace("a", "follow-up", "follow-up", WorkspaceState::Ready),
        ]);
        workspaces[6].review = vec![mark(false)];
        workspaces[6].agent_state = agent(AgentState::Waiting);
        workspaces[7].review = vec![mark(false)];
        workspaces[7].agent_state = agent(AgentState::Working);
        workspaces[8].running = true;
        workspaces[8].agent_state = agent(AgentState::Idle);
        workspaces[9].done = true;
        workspaces[9].review = vec![mark(false)];
        workspaces[9].agent_state = agent(AgentState::Idle);
        workspaces[10].done = true;
        workspaces[10].agent_state = agent(AgentState::Waiting);
        assert_eq!(
            workspace_rows(
                &workspaces,
                &[("a".into(), "shoal".into())],
                &HashSet::from([workspaces[0].id.clone(), workspaces[1].id.clone()]),
                plain,
                None,
            ),
            [
                "● fix-login  stopped",
                "✗ x          failed",
                "◌ new        preparing",
                "◐ busy       process running   Refine the list view",
                "● review     ready",
                "○ quiet      ready (outdated)",
                "● asking     needs input",
                "● marked     ready",
                "○ agent      waiting",
                "○ done       finished",
                "● follow-up  needs input",
            ]
        );
        let repositories = [("a".into(), "shoal".into()), ("b".into(), "app".into())];
        let workspaces = [
            workspace("a", "fix-login", "fix-login", WorkspaceState::Ready),
            workspace("b", "y", "y", WorkspaceState::Ready),
        ];
        assert_eq!(
            workspace_rows(&workspaces, &repositories, &HashSet::new(), plain, None),
            [
                "○ fix-login  shoal  inactive",
                "○ y          app    inactive"
            ]
        );
    }
}
