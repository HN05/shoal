//! The requests for other joined machines' spaces that the hub holds while
//! their machine is offline.
use anyhow::Result;
use shoal_protocol::hub::routing::Queued;

use crate::cli::{context::Context, output::Style};

/// Report that the hub holds the command's request for the offline machine
/// of its space, which is its outcome until that machine connects.
pub(super) fn queued(ctx: &Context, queued: &Queued) -> Result<i32> {
    ctx.emit_styled(Style::Warning, &queued.to_string(), queued)?;
    Ok(0)
}
