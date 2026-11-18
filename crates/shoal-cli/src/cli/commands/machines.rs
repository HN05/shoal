//! The other joined machines' spaces, which `shoal list`, `inbox` and
//! `status` show beside this machine's, and the requests for them that the
//! hub holds while their machine is offline.
use anyhow::{Result, bail};
use shoal_protocol::{
    hub::{
        directory::{HubView, Listed},
        routing::Queued,
    },
    space_names::SpaceSelector,
};

use crate::cli::{context::Context, output::Style};

/// The hub's view of the other machines' spaces the caller sees; none on a
/// machine that takes no part in a hub, and none, with a warning, while the
/// hub cannot be asked.
pub(super) async fn view(ctx: &Context) -> Option<HubView> {
    match crate::client::hub::directory(&ctx.paths).await {
        Ok(view) => view,
        Err(error) => {
            eprintln!("warning: the other machines' spaces are left out: {error:#}");
            None
        }
    }
}

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
    let Some(view) = view(ctx).await else {
        return Ok(None);
    };
    let mut found = (view.directory.spaces.iter())
        .filter(|remote| selector.selects(&remote.space.id, remote.space.number));
    match (found.next().cloned(), found.next()) {
        (Some(remote), None) => Ok(Some((remote, view))),
        (Some(_), Some(_)) => bail!(
            "several other machines have a space {selector}; name it by the ID shoal list --json shows"
        ),
        (None, _) => Ok(None),
    }
}

/// Report that the hub holds the command's request for the offline machine
/// of its space, which is its outcome until that machine connects.
pub(super) fn queued(ctx: &Context, queued: &Queued) -> Result<i32> {
    ctx.emit_styled(Style::Warning, &queued.to_string(), queued)?;
    Ok(0)
}
