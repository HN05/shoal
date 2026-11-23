//! `shoal review cancel`: cancel a queued or running review, or drop a
//! pending one with its comments.
use anyhow::{Context as _, Result};

use crate::client::reviews;
use crate::{
    cli::{context::Context, ui},
    model::Chat,
    reviews::ReviewId,
};
use shoal_protocol::space_names::SpaceSelector;

/// Cancel review `id`, else the caller's pending review of the space named
/// or this runs in, or its queued or running one, a chat's that of one of
/// its linked PRs; outside every space, the caller's only pending review.
pub(super) async fn cancel(
    ctx: &Context,
    id: Option<ReviewId>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let space = match (id, space) {
        (None, None) => ui::space_here(ctx, ui::Workspaces::Below).await?,
        (_, space) => space,
    };
    let pending = match (id, &space) {
        (None, None) => super::writing::only_pending(ctx).await?,
        _ => None,
    };
    let (workspace, id) = match id.or(pending) {
        Some(id) => (None, Some(id)),
        None => cancelled(ctx, space).await?,
    };
    let review = reviews::cancel(workspace, id).send(&ctx.paths).await?;
    ctx.emit(&format!("Review {} cancelled.", review.id), &review)?;
    Ok(0)
}

/// What a cancellation without an ID names: a workspace, whose pending,
/// queued or running review the daemon finds, or the queued or running
/// review of a chat's linked PRs, else the caller's only pending review.
async fn cancelled(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<(Option<SpaceSelector>, Option<ReviewId>)> {
    Ok(match super::select(ctx, space).await? {
        super::Space::Chat(chat) => (None, Some(chat_cancelled(ctx, &chat).await?)),
        super::Space::Workspace(workspace) => (Some(workspace), None),
    })
}

/// The review a cancellation in or of `chat` ends: the queued or running
/// review of its linked PRs, else the caller's only pending review.
async fn chat_cancelled(ctx: &Context, chat: &Chat) -> Result<ReviewId> {
    if let Some(id) = active_review(ctx, chat).await? {
        return Ok(id);
    }
    let pending = super::writing::only_pending(ctx).await?;
    pending.with_context(|| {
        format!(
            "no review of {}'s PRs is queued or running, nor is any one review of yours pending",
            chat.name
        )
    })
}

/// The ID of the queued or running review of the chat's linked PRs.
async fn active_review(ctx: &Context, chat: &Chat) -> Result<Option<ReviewId>> {
    let reviews = super::chat_reviews(ctx, chat).await?;
    let active = (reviews.into_iter()).find(|review| review.state.is_active());
    Ok(active.map(|review| review.id))
}
