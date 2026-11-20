//! The other joined machines' spaces a picker offers after this machine's,
//! for a command that acts on them there, each marked with its machine as
//! `shoal list` marks it, with when the hub last saw one that is offline.
use shoal_protocol::hub::directory::{HubView, SpaceSummary};

use super::{Entries, Machines, picker_width, remote_rows};
use crate::{
    cli::{context::Context, hub_view, output::Palette, reach},
    model::SpaceKind,
};

/// Which of the other machines' spaces a picker offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    /// Their workspaces.
    Workspaces,
    /// Their workspaces and the chats whose agent runs.
    RunningChats,
    /// Their workspaces and chats.
    Spaces,
}

impl Offered {
    fn offers(self, space: &SpaceSummary) -> bool {
        match (self, space.kind) {
            (_, SpaceKind::Workspace) | (Self::Spaces, SpaceKind::Chat) => true,
            (Self::RunningChats, SpaceKind::Chat) => space.running.is_some(),
            (Self::Workspaces, SpaceKind::Chat) => false,
        }
    }
}

/// The picker entries of the other machines' spaces `offered` names, for
/// a command that acts on them; none for another command or a machine in
/// no hub, and none, with a warning, while the hub cannot be asked.
pub(super) async fn elsewhere_entries(ctx: &Context, offered: Offered) -> Entries {
    if !reach::reaches_elsewhere(&ctx.command) {
        return Entries::new();
    }
    let Some(view) = hub_view::view(ctx).await else {
        return Entries::new();
    };
    view_entries(&view, offered, Palette::stderr(ctx.json), picker_width())
}

/// The entries of the spaces of `view` that `offered` names, by their IDs.
fn view_entries(
    view: &HubView,
    offered: Offered,
    palette: Palette,
    width: Option<usize>,
) -> Entries {
    let spaces: Vec<_> = (view.directory.spaces.iter())
        .filter(|listed| offered.offers(&listed.space))
        .collect();
    let rows = remote_rows(&spaces, &Machines::new(view), palette, width);
    (spaces.iter())
        .map(|listed| listed.space.id.to_string())
        .zip(rows)
        .collect()
}

#[cfg(test)]
mod tests;
