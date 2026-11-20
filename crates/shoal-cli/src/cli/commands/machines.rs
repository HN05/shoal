//! The other joined machines' spaces, which `shoal list`, `inbox` and
//! `status` show beside this machine's, and the requests for them that the
//! hub holds while their machine is offline.
use anyhow::Result;
use shoal_protocol::{
    hub::{
        directory::{HubView, Listed},
        routing::Queued,
    },
    space_names::SpaceSelector,
};

use crate::cli::{context::Context, hub_view, output::Style};

/// The other machine's space `selector` names when no space of this
/// machine has it: by its ID, or by a number only one other machine's
/// space has, with the hub's view that names its machine.
pub(super) async fn space_elsewhere(
    ctx: &Context,
    selector: &SpaceSelector,
) -> Result<Option<(Listed, HubView)>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    if spaces.find(selector).is_some() {
        return Ok(None);
    }
    let Some(view) = hub_view::view(ctx).await else {
        return Ok(None);
    };
    let selected = view
        .selected(selector)
        .map_err(anyhow::Error::msg)?
        .cloned();
    Ok(selected.map(|remote| (remote, view)))
}

/// Report that the hub holds the command's request for the offline machine
/// of its space, which is its outcome until that machine connects.
pub(super) fn queued(ctx: &Context, queued: &Queued) -> Result<i32> {
    ctx.emit_styled(Style::Warning, &queued.to_string(), queued)?;
    Ok(0)
}
