//! Lists of spaces as one tree: each space below its listed parent.
use std::collections::HashSet;

use super::{
    Cell, Statuses, aligned_to, chat_row, indented, mode_cell, number_cell, progress_cell,
    queue_subject, statuses::place_cell, title_limit, usage_cell, workspace_mode_cell,
    workspace_status_cell, workspace_text,
};
use crate::cli::list_status::ListStatus;
use crate::cli::output::{Palette, Style};
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::{SpaceId, SpaceNumber};
use crate::config::modes::ModeName;
use crate::model::{Chat, QueueEntry, Workspace};

/// `workspace_rows` for workspaces, the running and stopped chats and the
/// agents queued to start in a workspace without a row in one tree: the
/// spaces whose parent is not listed, each followed by the spaces below it,
/// indented, and spaces under one parent by their status, then their number.
/// A chat row names the chat, its status and its agent, and a queued agent's
/// row what it starts. Workspace and queued agent rows name their
/// repository, of `(id, name)` pairs, when the rows span several.
pub fn workspace_tree_rows(
    workspaces: &[Workspace],
    chats: &[Chat],
    statuses: &Statuses,
    repositories: &[(String, String)],
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let tree = SpaceTree::new(workspaces, chats, statuses, repositories, palette);
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

/// A listed space, or an agent queued to start in a workspace without a
/// row: a new one, or one the list leaves out.
#[derive(Clone, Copy)]
enum Listed<'a> {
    Workspace(&'a Workspace),
    Chat(&'a Chat),
    Queued(&'a QueueEntry),
}

impl<'a> Listed<'a> {
    /// The space's ID; a queued agent's row stands for none.
    fn id(&self) -> Option<&'a SpaceId> {
        match self {
            Self::Workspace(workspace) => Some(&workspace.id),
            Self::Chat(chat) => Some(&chat.id),
            Self::Queued(_) => None,
        }
    }

    fn repository(&self) -> Option<&'a RepositoryId> {
        match self {
            Self::Workspace(workspace) => Some(&workspace.repository_id),
            Self::Chat(chat) => chat.repository_id.as_ref(),
            Self::Queued(entry) => Some(&entry.repository_id),
        }
    }

    fn number(&self) -> Option<SpaceNumber> {
        match self {
            Self::Workspace(workspace) => Some(workspace.number),
            Self::Chat(chat) => Some(chat.number),
            Self::Queued(_) => None,
        }
    }

    /// The space it lists under: its parent, or for a CI job's workspace,
    /// which has none, and a queued CI job, the first space its run is for.
    fn parent(&self) -> Option<&'a SpaceId> {
        let parent = match self {
            Self::Workspace(workspace) => workspace.parent.as_ref(),
            Self::Chat(chat) => chat.parent.as_ref(),
            Self::Queued(_) => None,
        };
        let job = match self {
            Self::Workspace(workspace) => workspace.job.as_ref(),
            Self::Queued(entry) => entry.job.as_ref(),
            Self::Chat(_) => None,
        };
        let linked = job.and_then(|job| job.space_ids.first());
        parent.map(|parent| &parent.id).or(linked)
    }
}

/// Listed spaces arranged under their listed parents.
struct SpaceTree<'a> {
    /// Every listed space with its status, in the order lists show them:
    /// queued agents, whose rows have no number, after the numbered spaces
    /// of their status.
    spaces: Vec<(Listed<'a>, ListStatus)>,
    statuses: &'a Statuses,
    /// The `(id, name)` pairs of the repositories rows name; none while the
    /// listed spaces share one.
    repositories: &'a [(String, String)],
    palette: Palette,
}

impl<'a> SpaceTree<'a> {
    fn new(
        workspaces: &'a [Workspace],
        chats: &'a [Chat],
        statuses: &'a Statuses,
        repositories: &'a [(String, String)],
        palette: Palette,
    ) -> Self {
        let listed: HashSet<&SpaceId> = workspaces.iter().map(|workspace| &workspace.id).collect();
        let workspaces = (workspaces.iter())
            .map(|workspace| (Listed::Workspace(workspace), statuses.of(workspace)));
        let chats = (chats.iter()).map(|chat| (Listed::Chat(chat), ListStatus::of_chat(chat)));
        let queued = (statuses.queued.iter())
            .filter(|entry| entry.space().is_none_or(|id| !listed.contains(id)))
            .map(|entry| (Listed::Queued(entry), ListStatus::of_entry(entry)));
        let mut spaces: Vec<_> = workspaces.chain(chats).chain(queued).collect();
        spaces.sort_by_key(|(space, status)| (*status, space.number().is_none(), space.number()));
        let named: HashSet<_> = (spaces.iter())
            .filter_map(|(space, _)| space.repository())
            .collect();
        Self {
            repositories: if named.len() > 1 { repositories } else { &[] },
            spaces,
            statuses,
            palette,
        }
    }

    /// The name of the repository of `space`, when rows name one.
    fn repository_cell(&self, space: Listed<'a>) -> Cell {
        let named = space
            .repository()
            .and_then(|id| (self.repositories.iter()).find(|(repository, _)| id == repository));
        (
            named.map(|(_, name)| name.clone()).unwrap_or_default(),
            None,
        )
    }

    /// The spaces whose parent is not listed.
    fn roots(&self) -> Vec<(Listed<'a>, ListStatus)> {
        let listed: HashSet<&SpaceId> = (self.spaces.iter())
            .filter_map(|(space, _)| space.id())
            .collect();
        (self.spaces.iter().copied())
            .filter(|(space, _)| space.parent().is_none_or(|parent| !listed.contains(parent)))
            .collect()
    }

    /// The row of `space` at `depth`, then those of the spaces below it.
    fn push(
        &self,
        rows: &mut Vec<(String, [Cell; 8])>,
        (space, status): (Listed<'a>, ListStatus),
        depth: usize,
    ) {
        let repository = self.repository_cell(space);
        rows.push(match space {
            Listed::Workspace(workspace) => {
                let entries = self.statuses.entries(workspace);
                let status = (status, &entries[..]);
                tree_workspace_row(workspace, status, repository, depth, self.palette)
            }
            Listed::Chat(chat) => chat_row(chat, depth, self.palette),
            Listed::Queued(entry) => queued_row(entry, status, repository, depth, self.palette),
        });
        let Some(id) = space.id() else {
            return;
        };
        let children = self.spaces.iter().copied();
        for child in children.filter(|(child, _)| child.parent() == Some(id)) {
            self.push(rows, child, depth + 1);
        }
    }
}

/// A workspace's row, with its status and the agents queued in it.
fn tree_workspace_row(
    workspace: &Workspace,
    (status, entries): (ListStatus, &[&QueueEntry]),
    repository: Cell,
    depth: usize,
    palette: Palette,
) -> (String, [Cell; 8]) {
    let name = indented(&workspace.name, depth);
    (
        status.marker(palette),
        [
            number_cell(workspace.number),
            (name, Some(Style::Heading)),
            repository,
            workspace_mode_cell(workspace),
            workspace_status_cell(workspace, status, entries),
            usage_cell(workspace),
            progress_cell(workspace.progress.as_ref()),
            (workspace_text(workspace), None),
        ],
    )
}

/// The row of an agent queued to start in a workspace without a row: the
/// workspace's name, its repository, the agent's mode, its place in the
/// queue, and what it starts.
fn queued_row(
    entry: &QueueEntry,
    status: ListStatus,
    repository: Cell,
    depth: usize,
    palette: Palette,
) -> (String, [Cell; 8]) {
    (
        status.marker(palette),
        [
            (String::new(), None),
            (indented(&entry.name, depth), Some(Style::Heading)),
            repository,
            mode_cell(entry.mode.as_ref(), &ModeName::DEFAULT),
            place_cell(status, &[entry]),
            (String::new(), None),
            (String::new(), None),
            (queue_subject(entry), None),
        ],
    )
}

#[cfg(test)]
mod tests;
