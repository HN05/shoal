//! The other joined machines' spaces a picker offers after this machine's,
//! for a command that acts on them there, each marked with its machine as
//! `shoal list` marks it, with when the hub last saw one that is offline.
use shoal_protocol::hub::directory::{HubView, Offered};

use super::{Acting, Entries, Machines, picker_width, remote_rows};
use crate::cli::{availability::Need, context::Context, everywhere, output::Palette};

/// The picker entries of the other machines' spaces `offered` names that
/// `acting`, what the command does with the picked space, can act on; none
/// for a command that acts here or on a machine in no hub, and none, with
/// a warning, while the hub cannot be asked.
pub(super) async fn elsewhere_entries(ctx: &Context, offered: Offered, acting: &Acting) -> Entries {
    let Some(acting) = acting.operation() else {
        return Entries::new();
    };
    let offered_view = everywhere::offered(ctx, acting, "the other machines' spaces");
    let Some(view) = offered_view.await else {
        return Entries::new();
    };
    let palette = Palette::stderr(ctx.json);
    view_entries(&view, offered, ctx.need, palette, picker_width())
}

/// The entries of the spaces of `view` that `offered` names and `need`
/// offers, by their IDs, in `palette` cut to `width`.
fn view_entries(
    view: &HubView,
    offered: Offered,
    need: Need,
    palette: Palette,
    width: Option<usize>,
) -> Entries {
    let spaces: Vec<_> = (view.directory.spaces.iter())
        .filter(|listed| offered.offers(&listed.space) && need.offers_elsewhere(&listed.space))
        .collect();
    let rows = remote_rows(&spaces, &Machines::new(view), palette, width);
    (spaces.iter())
        .map(|listed| listed.space.id.to_string())
        .zip(rows)
        .collect()
}

#[cfg(test)]
mod tests;
