//! `--permission` on `shoal resume`: a change to the permissions the resumed
//! agents and chats and the failed launches queued again were given. A
//! scoped process gives only what it holds, which is checked for every
//! selected space before anything resumes; `shoal_workers::resume`'s
//! `permissions` makes the change once nothing refuses it.
use anyhow::{Context as _, Result};

use super::targets::Selected;
use crate::{cli::context::Context, config::permissions::PermissionChange};

/// Refuse `change` for the stopped agents and last sessions of the
/// `selected` workspaces, the selected chats and the selected failed
/// launches, where this process does not hold what it gives. It writes
/// nothing: a resume makes the change once nothing refuses it.
pub(super) async fn check(
    ctx: &Context,
    selected: &Selected,
    change: &PermissionChange,
) -> Result<()> {
    let selectors = (selected.spaces.as_deref())
        .context("--permission changes the permissions of the spaces a resume names")?;
    for selector in selectors {
        let space = super::present_space(ctx, selector).await?;
        let repository = space.repository_id.as_ref().map(|id| id.as_str().into());
        crate::cli::commands::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    for failed in &selected.failed_starts {
        let repository = failed.repository_id.as_ref().map(|id| id.as_str().into());
        crate::cli::commands::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}
