//! The spaces a package carries with its space, which its import recorded
//! as removed here: this opens each again after it, parents first, as
//! `shoal restore` does, stopped or with their agents' restores queued.
use anyhow::{Context as _, Result};

use super::super::reopen::{self, ReopenedExt};
use crate::{cli::context::Context, transfer::CarriedRecord};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// Open each space the import `carried` recorded again, in order, queueing
/// their agents' restores when `restore`; the failure of each space that
/// did not open, whose record, when the import made one, stays for `shoal
/// restore`.
pub(super) async fn open(
    ctx: &Context,
    carried: Vec<CarriedRecord>,
    restore: bool,
) -> Vec<anyhow::Error> {
    let mut failed = Vec::new();
    for record in carried {
        match record {
            CarriedRecord::Recorded {
                name,
                space,
                warnings,
            } => {
                for warning in &warnings {
                    eprintln!("warning: {warning}");
                }
                if let Err(error) = open_one(ctx, &space, restore).await {
                    let retry = format!("cannot open {name} again; shoal restore {space} retries");
                    failed.push(error.context(retry));
                }
            }
            CarriedRecord::Failed { name, error } => {
                failed.push(anyhow::anyhow!("cannot open {name} again: {error}"));
            }
        }
    }
    failed
}

/// Refuse an import whose carried spaces `failed` to open, so a move keeps
/// what did not arrive.
pub(super) fn ensure_opened(failed: Vec<anyhow::Error>) -> Result<()> {
    if failed.is_empty() {
        return Ok(());
    }
    let failures: Vec<String> = failed.iter().map(|error| format!("{error:#}")).collect();
    anyhow::bail!(
        "the space opened, but not every space it carries:\n  {}",
        failures.join("\n  ")
    )
}

/// Open the recorded space `id` again as [`open`] does, and report it.
async fn open_one(ctx: &Context, id: &SpaceId, restore: bool) -> Result<()> {
    let restored = crate::client::removal::restore(&ctx.paths, SpaceSelector::id(id)).await?;
    let mut reopened = reopen::prepare_set_up(ctx, restored).await?;
    let post_setup = reopen::run_post_setup(ctx, &reopened).await;
    if post_setup.is_ok() && restore {
        reopened.queue_agents(ctx).await;
    }
    if !ctx.json {
        outln!("{}", reopened.message(ctx, "Imported"));
    }
    post_setup.context("it opened, but its hooks.post_setup failed")
}
