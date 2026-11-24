//! `--focus`: resuming one space's agent in this terminal, which only this
//! process can do: its failed launch, else its stopped agent or last
//! session, the `--execution` one when given.
use anyhow::{Result, bail};

use super::{Options, Resuming, failed_starts, reports};
use crate::cli::context::Context;
use crate::client::output::resume::Queued;

/// Resume the agent of the one space `resuming` lists in this terminal as
/// `options` says, after reporting the spaces reopened for it.
pub(super) async fn here(ctx: &Context, resuming: Resuming, options: Options) -> Result<i32> {
    let [space] = resuming.spaces.as_slice() else {
        bail!("--focus resumes the agent of one space");
    };
    if !resuming.reopened.is_empty() {
        let reopened = Queued {
            reopened: resuming.reopened,
            ..Queued::default()
        };
        reports::queued(ctx, &reopened)?;
    }
    let present = super::space::present_space(ctx, &space.selector).await?;
    shoal_workers::resume::space::refuse_failed_setup(&ctx.paths, &present).await?;
    let change = options.change.as_ref();
    if options.execution.is_none()
        && let Some(failed) = failed_starts::of(ctx, &space.selector).await?
    {
        return failed_starts::retry_here(ctx, &failed, change).await;
    }
    super::space::here(ctx, space.selector.clone(), options.execution, change).await
}
