//! `shoal review cancel`, `reply` and `resolve`: act on a review and answer
//! its findings.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    protocol::ReviewMethod,
    reviews::{FindingId, Outcome, Review},
};

/// Cancel review `id`, else the workspace's queued or running one.
pub(super) async fn cancel(
    ctx: &Context,
    id: Option<i64>,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = match id {
        Some(_) => None,
        None => Some(ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?),
    };
    let review: Box<Review> =
        client::request(&ctx.paths, ReviewMethod::CancelReview { workspace, id }).await?;
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
    let review: Box<Review> = client::request(
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
        Some(outcome) => format!("Resolved finding {finding}: {}.", outcome.describe()),
        None => format!("Reopened finding {finding}."),
    };
    let review: Box<Review> = client::request(
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
