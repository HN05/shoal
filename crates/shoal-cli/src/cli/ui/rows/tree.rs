//! Lists of spaces as one tree: each space below its listed parent.
use std::collections::HashSet;

use super::{
    Cell, aligned_to, chat_row, indented, number_cell, progress_cell, title_limit, usage_cell,
    workspace_mode_cell, workspace_status_cell, workspace_text,
};
use crate::cli::list_status::{ListStatus, ListStatusExt};
use crate::cli::output::{Palette, Style};
use crate::client::space_ids::SpaceId;
use crate::model::{Chat, Workspace};

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

#[cfg(test)]
mod tests;
