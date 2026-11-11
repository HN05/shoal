//! What a `--permission` may give: a scoped process gives only what its own
//! space holds, the user any.
use anyhow::Result;

use crate::{cli::context::Context, config::permissions::PermissionsConfig};

/// Refuse `given` where this scoped process does not hold it for an action
/// in the registered repository `repository`; the user, without a scope,
/// gives any.
pub(super) async fn ensure_held(
    ctx: &Context,
    repository: Option<crate::repository::Selector>,
    given: &PermissionsConfig,
) -> Result<()> {
    if !crate::env::is_scoped() || given.is_empty() {
        return Ok(());
    }
    let held = crate::client::workspaces::held_permissions(&ctx.paths, repository).await?;
    given.ensure_held(&held)
}
