//! Lists of spaces as one tree: each space below its listed parent.
use std::collections::HashSet;

use super::{
    Cell, Machines, Statuses, WidthLimit, aligned_to, chat_row, indented, mode_cell, number_cell,
    progress_cell, queue_subject, space_mode_cell, status_cell, status_line, statuses::place_cell,
    title_limit, usage_cell, workspace_mode_cell, workspace_status_cell, workspace_text,
};
use crate::cli::list_status::{ListStatus, ListStatusStyle};
use crate::cli::output::{Palette, Style};
use crate::config::modes::ModeName;
use crate::model::{Chat, QueueEntry, Workspace};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::hub::directory::Listed as Remote;

/// The spaces a tree lists: this machine's workspaces and chats, and the
/// other machines' spaces with the machines rows name, if any.
pub struct Listing<'a> {
    pub workspaces: &'a [Workspace],
    pub chats: &'a [Chat],
    pub elsewhere: Option<Elsewhere<'a>>,
}

/// The other machines' spaces a tree lists, and the machines its rows name.
pub struct Elsewhere<'a> {
    pub spaces: Vec<&'a Remote>,
    pub machines: Machines<'a>,
}

/// `workspace_rows` for the workspaces, the running and stopped chats and
/// the other machines' spaces of `listing`, and the agents queued to start
/// in a workspace without a row, in one tree: the spaces whose parent is not
/// listed, on any machine, each followed by the spaces below it, indented,
/// and spaces under one parent by their status, then their number. A chat
/// row names the chat, its status and its agent, and a queued agent's row
/// what it starts. Workspace, queued agent and other machines' rows name
/// their repository, of `(id, name)` pairs for this machine's, when the rows
/// span several, and once the tree lists other machines' spaces, every row
/// names its machine after its name.
pub fn space_tree_rows(
    listing: Listing<'_>,
    statuses: &Statuses,
    repositories: &[(String, String)],
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let tree = SpaceTree::new(&listing, statuses, repositories, palette);
    let mut rows = Vec::new();
    for root in tree.roots() {
        tree.push(&mut rows, root, 0);
    }
    let (markers, cells): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
    // The machine column comes before the title.
    let limit = title_limit(width).map(|limit| WidthLimit {
        column: limit.column + 1,
        ..limit
    });
    markers
        .into_iter()
        .zip(aligned_to(cells, palette, limit))
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
    /// A space of another machine.
    Elsewhere(&'a Remote),
}

impl<'a> Listed<'a> {
    /// The space's ID; a queued agent's row stands for none.
    fn id(&self) -> Option<&'a SpaceId> {
        match self {
            Self::Workspace(workspace) => Some(&workspace.id),
            Self::Chat(chat) => Some(&chat.id),
            Self::Queued(_) => None,
            Self::Elsewhere(remote) => Some(&remote.space.id),
        }
    }

    /// The name of its repository, of `(id, name)` pairs for this
    /// machine's.
    fn repository(&self, repositories: &'a [(String, String)]) -> Option<&'a str> {
        let id = match self {
            Self::Workspace(workspace) => workspace.repository_id.as_ref(),
            Self::Chat(chat) => chat.repository_id.as_ref(),
            Self::Queued(entry) => entry.repository_id.as_ref(),
            Self::Elsewhere(remote) => {
                return (remote.space.repository.as_ref()).map(|origin| origin.name.as_str());
            }
        }?;
        let named = repositories.iter().find(|(repository, _)| id == repository);
        named.map(|(_, name)| name.as_str())
    }

    fn number(&self) -> Option<SpaceNumber> {
        match self {
            Self::Workspace(workspace) => Some(workspace.number),
            Self::Chat(chat) => Some(chat.number),
            Self::Queued(_) => None,
            Self::Elsewhere(remote) => Some(remote.space.number),
        }
    }

    /// The space it lists under: its parent, or for a CI job's workspace,
    /// which has none, and a queued CI job, the first space its run is for.
    fn parent(&self) -> Option<&'a SpaceId> {
        let parent = match self {
            Self::Workspace(workspace) => workspace.parent.as_ref(),
            Self::Chat(chat) => chat.parent.as_ref(),
            Self::Queued(_) => None,
            Self::Elsewhere(remote) => return remote.space.parent.as_ref(),
        };
        let job = match self {
            Self::Workspace(workspace) => workspace.job.as_ref(),
            Self::Queued(entry) => entry.job.as_ref(),
            Self::Chat(_) | Self::Elsewhere(_) => None,
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
    /// The `(id, name)` pairs of the repositories of this machine's spaces.
    repositories: &'a [(String, String)],
    /// Whether rows name their repository, as they do once the listed
    /// spaces span several.
    names_repositories: bool,
    /// The machines rows name, once the tree lists another machine's
    /// spaces.
    machines: Option<&'a Machines<'a>>,
    palette: Palette,
}

impl<'a> SpaceTree<'a> {
    fn new(
        listing: &'a Listing<'a>,
        statuses: &'a Statuses,
        repositories: &'a [(String, String)],
        palette: Palette,
    ) -> Self {
        let Listing {
            workspaces,
            chats,
            elsewhere,
        } = listing;
        let listed: HashSet<&SpaceId> = workspaces.iter().map(|workspace| &workspace.id).collect();
        let workspaces = (workspaces.iter())
            .map(|workspace| (Listed::Workspace(workspace), statuses.of(workspace)));
        let chats = (chats.iter()).map(|chat| (Listed::Chat(chat), ListStatus::of_chat(chat)));
        let queued = (statuses.queued.iter())
            .filter(|entry| entry.space().is_none_or(|id| !listed.contains(id)))
            .map(|entry| (Listed::Queued(entry), ListStatus::of_entry(entry)));
        let remote = (elsewhere
            .iter()
            .flat_map(|elsewhere| elsewhere.spaces.iter()))
        .map(|remote| (Listed::Elsewhere(remote), remote.space.status));
        let mut spaces: Vec<_> = (workspaces.chain(chats).chain(queued).chain(remote)).collect();
        spaces.sort_by_key(|(space, status)| (*status, space.number().is_none(), space.number()));
        let named: HashSet<_> = (spaces.iter())
            .filter_map(|(space, _)| space.repository(repositories))
            .collect();
        let machines = (elsewhere.as_ref())
            .filter(|elsewhere| !elsewhere.spaces.is_empty())
            .map(|elsewhere| &elsewhere.machines);
        Self {
            names_repositories: named.len() > 1,
            repositories,
            machines,
            spaces,
            statuses,
            palette,
        }
    }

    /// The name of the repository of `space`, when rows name one.
    fn repository_cell(&self, space: Listed<'a>) -> Cell {
        let named = (space.repository(self.repositories)).filter(|_| self.names_repositories);
        (named.unwrap_or_default().to_owned(), None)
    }

    /// The machine of `space`, when rows name one.
    fn machine_cell(&self, space: Listed<'a>) -> Cell {
        match (self.machines, space) {
            (None, _) => (String::new(), None),
            (Some(machines), Listed::Elsewhere(remote)) => machines.cell(&remote.machine),
            (Some(machines), _) => machines.here(),
        }
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
        rows: &mut Vec<(String, [Cell; 9])>,
        (space, status): (Listed<'a>, ListStatus),
        depth: usize,
    ) {
        let repository = self.repository_cell(space);
        let (marker, cells) = match space {
            Listed::Workspace(workspace) => {
                let entries = self.statuses.entries(workspace);
                let status = (status, &entries[..]);
                tree_workspace_row(workspace, status, repository, depth, self.palette)
            }
            Listed::Chat(chat) => chat_row(chat, depth, self.palette),
            Listed::Queued(entry) => queued_row(entry, status, repository, depth, self.palette),
            Listed::Elsewhere(remote) => remote_row(remote, repository, depth, self.palette),
        };
        rows.push((marker, with_machine(cells, self.machine_cell(space))));
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

/// The row of another machine's space, as its machine last reported it.
fn remote_row(
    remote: &Remote,
    repository: Cell,
    depth: usize,
    palette: Palette,
) -> (String, [Cell; 8]) {
    let space = &remote.space;
    (
        space.status.marker(palette),
        [
            number_cell(space.number),
            (indented(&space.name, depth), Some(Style::Heading)),
            repository,
            space_mode_cell(space.kind, space.mode.as_ref()),
            status_cell(space.status),
            (String::new(), None),
            progress_cell(space.progress.as_ref()),
            (
                status_line(space.progress.as_ref())
                    .unwrap_or_default()
                    .to_owned(),
                None,
            ),
        ],
    )
}

/// `cells` with `machine` after the space's name.
fn with_machine([number, name, rest @ ..]: [Cell; 8], machine: Cell) -> [Cell; 9] {
    let [a, b, c, d, e, f] = rest;
    [number, name, machine, a, b, c, d, e, f]
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
