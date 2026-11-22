//! The spaces the caller sees on every machine, as the daemon lists them:
//! this machine's, and the other joined machines' as the hub lists them,
//! which lists, the inbox and pickers show beside this machine's.
use anyhow::Result;
use shoal_protocol::{hub::directory::HubView, model::Everywhere, protocol::Operation};

use crate::cli::context::Context;

/// The spaces on every machine, warning that the other machines' are left
/// out while the daemon cannot ask the hub.
pub(in crate::cli) async fn spaces(ctx: &Context) -> Result<Everywhere> {
    let everywhere = crate::client::spaces::everywhere(&ctx.paths, true).await?;
    if let Some(error) = &everywhere.unreachable {
        eprintln!("warning: the other machines' spaces are left out: {error}");
    }
    Ok(everywhere)
}

/// The other machines' spaces, none on a machine in no hub, and none, with
/// a warning that what the caller reads of them, `left_out`, is left out,
/// while they cannot be listed.
pub(in crate::cli) async fn elsewhere(ctx: &Context, left_out: &str) -> Option<HubView> {
    let everywhere = crate::client::spaces::everywhere(&ctx.paths, false).await;
    listed_or_left_out(everywhere, left_out)
}

/// The other machines' spaces that `acting`, the request a picker offers
/// spaces for, can act on, as [`elsewhere`] lists them.
pub(in crate::cli) async fn offered(
    ctx: &Context,
    acting: Operation,
    left_out: &str,
) -> Option<HubView> {
    let everywhere = crate::client::spaces::offered_elsewhere(&ctx.paths, acting).await;
    listed_or_left_out(everywhere, left_out)
}

/// The other machines' spaces `everywhere` lists, none, with a warning that
/// `left_out` is left out, while they cannot be listed.
fn listed_or_left_out(everywhere: Result<Everywhere>, left_out: &str) -> Option<HubView> {
    let error = match everywhere {
        Ok(Everywhere {
            elsewhere,
            unreachable: None,
            ..
        }) => return elsewhere,
        Ok(Everywhere {
            unreachable: Some(error),
            ..
        }) => error,
        Err(error) => format!("{error:#}"),
    };
    eprintln!("warning: {left_out} are left out: {error}");
    None
}
