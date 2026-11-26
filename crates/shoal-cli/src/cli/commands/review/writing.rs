//! `shoal review start` and `submit`.
use anyhow::{Result, bail};

use super::super::items::content::read_body;
use crate::cli::args::BodyArgs;
use crate::cli::ui::{self, Acting, Fallback};
use crate::client::reviews;
use crate::{cli::context::Context, reviews::Review};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn start(ctx: &Context, space: Option<SpaceSelector>) -> Result<i32> {
    let starting = starting();
    let workspace = ui::select_workspace(ctx, space, Fallback::CurrentDirectory(&starting));
    let review = reviews::start(workspace.await?).send(&ctx.paths).await?;
    let message = format!(
        "Review {id} of {workspace} is pending. Threads you open towards {workspace} without \
         --kind are its comments; submit them together with shoal review submit, or drop them \
         with shoal review cancel {id}.",
        id = review.id,
        workspace = review.workspace,
    );
    ctx.emit(&message, &review)?;
    Ok(0)
}

pub(super) fn starting() -> Acting {
    Acting::request(reviews::start)
}

pub(super) async fn submit(
    ctx: &Context,
    space: Option<SpaceSelector>,
    approve: bool,
    body: BodyArgs,
) -> Result<i32> {
    let summary = read_body(body)?.map(|summary| summary.trim_end().to_owned());
    let space = match space {
        Some(space) => Some(space),
        None => pending_here(ctx).await?,
    };
    let review = reviews::submit(space, approve, summary)
        .send(&ctx.paths)
        .await?;
    let message = format!(
        "Review {} of {} is submitted: {}.",
        review.id,
        review.workspace,
        review.status()
    );
    ctx.emit(&message, &review)?;
    Ok(0)
}

/// The space this runs in when the caller has a pending review of it, which
/// a submit without a space submits before the caller's only one.
async fn pending_here(ctx: &Context) -> Result<Option<SpaceSelector>> {
    let Some(SpaceSelector::Id(here)) = ui::space_here(ctx, ui::Workspaces::Below).await? else {
        return Ok(None);
    };
    let pending = reviews::pending().send(&ctx.paths).await?;
    let reviewed = pending.iter().any(|review| review.workspace_id == here);
    Ok(reviewed.then_some(SpaceSelector::Id(here)))
}

/// The workspace of the caller's only pending review, which a cancel
/// outside every space drops there, on the machine that keeps it; none
/// without one.
pub(super) async fn only_pending(ctx: &Context) -> Result<Option<SpaceSelector>> {
    let pending = reviews::pending().send(&ctx.paths).await?;
    match pending.as_slice() {
        [] => Ok(None),
        [review] => Ok(Some(SpaceSelector::Id(review.workspace_id.clone()))),
        several => bail!(
            "you have pending reviews of {}; name the one to cancel",
            names(several)
        ),
    }
}

fn names(reviews: &[Review]) -> String {
    (reviews.iter())
        .map(|review| format!("{} (review {})", review.workspace, review.id))
        .collect::<Vec<_>>()
        .join(", ")
}
