//! `shoal review cancel`, `reply` and `resolve`: act on a review and answer
//! its findings.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::{
    cli::{client, context::Context},
    protocol::ReviewMethod,
    reviews::{FindingId, Outcome, Review},
};

/// Cancel review `id`, else the space's queued or running one: a chat's is
/// that of one of its linked PRs.
pub(super) async fn cancel(ctx: &Context, id: Option<i64>, space: Option<String>) -> Result<i32> {
    let (workspace, id) = match id {
        Some(id) => (None, Some(id)),
        None => match super::select(ctx, space).await? {
            super::Space::Chat(chat) => {
                let active = (super::chat_reviews(ctx, &chat).await?.into_iter())
                    .find(|review| review.state.is_active())
                    .with_context(|| {
                        format!("no review of {}'s PRs is queued or running", chat.name)
                    })?;
                (None, Some(active.id))
            }
            super::Space::Workspace(workspace) => (Some(workspace), None),
        },
    };
    let review: Review =
        client::reviews(&ctx.paths, ReviewMethod::CancelReview { workspace, id }).await?;
    ctx.emit(&format!("Review {} cancelled.", review.id), &review)?;
    Ok(0)
}

pub(super) async fn reply(
    ctx: &Context,
    finding: FindingId,
    text: Option<String>,
    body_file: Option<PathBuf>,
) -> Result<i32> {
    let body = match (text, body_file) {
        (Some(text), _) => text,
        (None, Some(path)) => {
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?
        }
        (None, None) => anyhow::bail!("give the reply as text or with --body-file"),
    };
    let review: Review = client::reviews(
        &ctx.paths,
        ReviewMethod::ReplyToFinding {
            workspace: None,
            finding,
            body,
        },
    )
    .await?;
    ctx.emit(&format!("Replied to finding {finding}."), &review)?;
    Ok(0)
}

/// Resolve `finding` with `outcome`, or reopen it without one.
pub(super) async fn resolve(
    ctx: &Context,
    finding: FindingId,
    outcome: Option<Outcome>,
) -> Result<i32> {
    let message = match &outcome {
        Some(_) => format!("Resolved finding {finding}."),
        None => format!("Reopened finding {finding}."),
    };
    let review: Review = client::reviews(
        &ctx.paths,
        ReviewMethod::ResolveFinding {
            workspace: None,
            finding,
            outcome,
        },
    )
    .await?;
    ctx.emit(&message, &review)?;
    Ok(0)
}
