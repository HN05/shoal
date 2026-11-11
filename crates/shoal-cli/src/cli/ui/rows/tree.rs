//! Lists of spaces as one tree: each space below its listed parent.
use std::collections::HashSet;

use super::{
    Cell, aligned_to, chat_row, indented, number_cell, progress_cell, title_limit, usage_cell,
    workspace_mode_cell, workspace_status_cell, workspace_text,
};
use crate::cli::list_status::{ListStatus, ListStatusExt};
use crate::cli::output::{Palette, Style};
use crate::client::space_ids::{SpaceId, SpaceNumber};
use crate::model::{Chat, Workspace};

/// `workspace_rows` for workspaces and the running and stopped chats in one
/// tree: the spaces whose parent is not listed, each followed by the spaces
/// below it, indented, and spaces under one parent by their status, then
/// their number. A chat row names the chat, its status and its agent.
pub fn workspace_tree_rows(
    workspaces: &[Workspace],
    chats: &[Chat],
    stopped: &HashSet<SpaceId>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let tree = SpaceTree::new(workspaces, chats, stopped, palette);
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

    fn number(&self) -> SpaceNumber {
        match self {
            Self::Workspace(workspace) => workspace.number,
            Self::Chat(chat) => chat.number,
        }
    }

    /// The space it lists under: its parent, or for a CI job's workspace,
    /// which has none, the first space its run is for.
    fn parent(&self) -> Option<&'a SpaceId> {
        let parent = match self {
            Self::Workspace(workspace) => workspace.parent.as_ref(),
            Self::Chat(chat) => chat.parent.as_ref(),
        };
        let linked = match self {
            Self::Workspace(workspace) => {
                workspace.job.as_ref().and_then(|job| job.space_ids.first())
            }
            Self::Chat(_) => None,
        };
        parent.map(|parent| &parent.id).or(linked)
    }
}

/// Listed spaces arranged under their listed parents.
struct SpaceTree<'a> {
    /// Every listed space with its status, in the order lists show them.
    spaces: Vec<(Listed<'a>, ListStatus)>,
    palette: Palette,
}

impl<'a> SpaceTree<'a> {
    fn new(
        workspaces: &'a [Workspace],
        chats: &'a [Chat],
        stopped: &HashSet<SpaceId>,
        palette: Palette,
    ) -> Self {
        let workspaces = workspaces.iter().map(|workspace| {
            let status = ListStatus::of_workspace(workspace, stopped.contains(&workspace.id));
            (Listed::Workspace(workspace), status)
        });
        let chats = (chats.iter()).map(|chat| (Listed::Chat(chat), ListStatus::of_chat(chat)));
        let mut spaces: Vec<_> = workspaces.chain(chats).collect();
        spaces.sort_by_key(|(space, status)| (*status, space.number()));
        Self { spaces, palette }
    }

    /// The spaces whose parent is not listed.
    fn roots(&self) -> Vec<(Listed<'a>, ListStatus)> {
        let listed: HashSet<&SpaceId> = self.spaces.iter().map(|(space, _)| space.id()).collect();
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
        rows.push(match space {
            Listed::Workspace(workspace) => {
                tree_workspace_row(workspace, status, depth, self.palette)
            }
            Listed::Chat(chat) => chat_row(chat, depth, self.palette),
        });
        let children = self.spaces.iter().copied();
        for child in children.filter(|(child, _)| child.parent() == Some(space.id())) {
            self.push(rows, child, depth + 1);
        }
    }
}

fn tree_workspace_row(
    workspace: &Workspace,
    status: ListStatus,
    depth: usize,
    palette: Palette,
) -> (String, [Cell; 8]) {
    let name = indented(&workspace.name, depth);
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

#[cfg(test)]
mod tests;
