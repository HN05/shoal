//! The spaces the caller sees on every machine: this machine's, and the
//! other joined machines' as the hub lists them.
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

/// The other machines' spaces, none on a machine in no hub. While they cannot
/// be listed, none, with a warning that `left_out` is left out.
pub(in crate::cli) async fn elsewhere(ctx: &Context, left_out: &str) -> Option<HubView> {
    let everywhere = crate::client::spaces::everywhere(&ctx.paths, false).await;
    listed_or_left_out(everywhere, left_out)
}

/// The spaces on every machine, with how `acting`, the request a picker
/// offers them for, judges each, warning that the other machines' are left
/// out while they cannot be listed, and that every space is offered while
/// the daemon cannot judge them.
pub(in crate::cli) async fn offered(ctx: &Context, acting: Operation) -> Everywhere {
    match crate::client::spaces::offered(&ctx.paths, acting).await {
        Ok(everywhere) => {
            if let Some(error) = &everywhere.unreachable {
                eprintln!("warning: the other machines' spaces are left out: {error}");
            }
            everywhere
        }
        Err(error) => {
            eprintln!("warning: the daemon cannot say which spaces this command takes: {error:#}");
            Everywhere::default()
        }
    }
}

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
