//! `shoal review cancel`: cancel a queued or running review, or drop a
//! pending one with its comments.
use anyhow::{Context as _, Result};

use crate::client::reviews::{self, PreparedReview};
use crate::{
    cli::{context::Context, ui},
    model::Chat,
    reviews::{Review, ReviewId},
};
use shoal_protocol::space_names::SpaceSelector;

/// Cancel review `id`, else the caller's pending review of the space named
/// or this runs in, or its queued or running one, a chat's that of one of
/// its linked PRs; in the space this runs in with nothing to cancel, or
/// outside every space, the caller's only pending review.
pub(super) async fn cancel(
    ctx: &Context,
    id: Option<ReviewId>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let cancelling = match (id, space) {
        (Some(id), _) => reviews::cancel(None, Some(id)),
        (None, Some(space)) => cancelled(ctx, Some(space), false).await?,
        (None, None) => match ui::space_here(ctx, ui::Workspaces::Below).await? {
            Some(here) => cancelled(ctx, Some(here), true).await?,
            None => match super::writing::only_pending(ctx).await? {
                Some(workspace) => reviews::cancel(Some(workspace), None),
                None => cancelled(ctx, None, false).await?,
            },
        },
    };
    let review = cancelling.send(&ctx.paths).await?;
    ctx.emit(&format!("Review {} cancelled.", review.id), &review)?;
    Ok(0)
}

/// What a cancellation without an ID sends for the space `space` names,
/// else one picked: for a workspace, its pending, queued or running review,
/// which the daemon finds, and for a chat, the queued or running review of
/// its linked PRs; `here` when it is the space this runs in, where with
/// nothing to cancel the caller's only pending review goes.
async fn cancelled(
    ctx: &Context,
    space: Option<SpaceSelector>,
    here: bool,
) -> Result<PreparedReview<Review>> {
    Ok(match (super::select(ctx, space).await?, here) {
        (super::Space::Chat(chat), here) => chat_cancelled(ctx, &chat, here).await?,
        (super::Space::Workspace(workspace), true) => reviews::cancel_here(workspace),
        (super::Space::Workspace(workspace), false) => reviews::cancel(Some(workspace), None),
    })
}

/// What a cancellation of `chat` sends: for the queued or running review
/// of its linked PRs, else, `here` in the chat, for the caller's only
/// pending review.
async fn chat_cancelled(ctx: &Context, chat: &Chat, here: bool) -> Result<PreparedReview<Review>> {
    if let Some(id) = active_review(ctx, chat).await? {
        return Ok(reviews::cancel(None, Some(id)));
    }
    let pending = match here {
        true => super::writing::only_pending(ctx).await?,
        false => None,
    };
    let workspace = pending
        .with_context(|| format!("no review of {}'s PRs is queued or running", chat.name))?;
    Ok(reviews::cancel(Some(workspace), None))
}

/// The ID of the queued or running review of the chat's linked PRs.
async fn active_review(ctx: &Context, chat: &Chat) -> Result<Option<ReviewId>> {
    let reviews = super::chat_reviews(ctx, chat).await?;
    let active = (reviews.into_iter()).find(|review| review.state.is_active());
    Ok(active.map(|review| review.id))
}
