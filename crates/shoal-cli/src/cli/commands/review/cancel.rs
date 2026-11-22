//! `shoal review cancel`: cancel a queued or running review.
use anyhow::{Context as _, Result};

use crate::client::reviews;
use crate::{cli::context::Context, model::Chat, reviews::ReviewId};
use shoal_protocol::space_names::SpaceSelector;

/// Cancel review `id`, else the space's queued or running one: a chat's is
/// that of one of its linked PRs.
pub(super) async fn cancel(
    ctx: &Context,
    id: Option<ReviewId>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let (workspace, id) = match id {
        Some(id) => (None, Some(id)),
        None => cancelled(ctx, space).await?,
    };
    let review = reviews::cancel(workspace, id).send(&ctx.paths).await?;
    ctx.emit(&format!("Review {} cancelled.", review.id), &review)?;
    Ok(0)
}

/// What a cancellation without an ID names: a workspace, whose queued or
/// running review the daemon finds, or the queued or running review of a
/// chat's linked PRs.
async fn cancelled(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<(Option<SpaceSelector>, Option<ReviewId>)> {
    Ok(match super::select(ctx, space).await? {
        super::Space::Chat(chat) => (None, Some(active_review(ctx, &chat).await?)),
        super::Space::Workspace(workspace) => (Some(workspace), None),
    })
}

/// The ID of the queued or running review of the chat's linked PRs.
async fn active_review(ctx: &Context, chat: &Chat) -> Result<ReviewId> {
    let reviews = super::chat_reviews(ctx, chat).await?;
    let active = (reviews.into_iter())
        .find(|review| review.state.is_active())
        .with_context(|| format!("no review of {}'s PRs is queued or running", chat.name))?;
    Ok(active.id)
}
