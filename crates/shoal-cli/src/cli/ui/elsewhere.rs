//! The other joined machines' spaces a picker offers after this machine's,
//! for a command that acts on them there, each marked with its machine as
//! `shoal list` marks it, with when the hub last saw one that is offline.
use shoal_protocol::hub::directory::{HubView, Offered};

use super::{
    Entries, Machines, judged_remote_rows,
    offers::{Offers, Shown},
    picker_width,
};
use crate::cli::{context::Context, output::Palette};

/// The picker entries of the other machines' spaces `offered` names, as
/// `offers` judges them; none for a command that acts here or on a machine
/// in no hub.
pub(super) fn elsewhere_entries(ctx: &Context, offered: Offered, offers: &Offers) -> Entries {
    let Some(view) = &offers.elsewhere else {
        return Entries::new();
    };
    let palette = Palette::stderr(ctx.json);
    view_entries(view, offered, offers, palette, picker_width())
}

/// The entries of the spaces of `view` that `offered` names and `offers`
/// offers, by their IDs, those the command would refuse last, in `palette`
/// cut to `width`.
fn view_entries(
    view: &HubView,
    offered: Offered,
    offers: &Offers,
    palette: Palette,
    width: Option<usize>,
) -> Entries {
    let mut spaces: Vec<_> = (view.directory.spaces.iter())
        .filter(|listed| offered.offers(&listed.space))
        .filter_map(|listed| match offers.shown(&listed.space.id)? {
            Shown::Taken => Some((listed, None)),
            Shown::Refused(reason) => Some((listed, Some(reason))),
        })
        .collect();
    spaces.sort_by_key(|(_, refusal)| refusal.is_some());
    let rows = judged_remote_rows(&spaces, &Machines::new(view), palette, width);
    (spaces.iter())
        .map(|(listed, _)| listed.space.id.to_string())
        .zip(rows)
        .collect()
}

#[cfg(test)]
mod tests;
