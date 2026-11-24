//! `--permission` on `shoal resume`: a change to the permissions the resumed
//! agents and chats and the failed launches queued again were given. A
//! scoped process gives only what it holds, which is checked for every
//! selected space before anything resumes; each space's daemon makes the
//! change as it resumes it.
use anyhow::{Result, ensure};

use crate::{cli::context::Context, config::permissions::PermissionChange};
use shoal_protocol::space_names::SpaceSelector;

/// Refuse `change` for the selected `spaces` where this process does not
/// hold what it gives, and for `--all`, which names none. It writes
/// nothing.
pub(super) async fn check(
    ctx: &Context,
    spaces: &[SpaceSelector],
    change: &PermissionChange,
    all: bool,
) -> Result<()> {
    ensure!(
        !all,
        "--permission changes the permissions of the spaces a resume names"
    );
    for selector in spaces {
        let space = super::space::present_space(ctx, selector).await?;
        let repository = space.repository_id.as_ref().map(|id| id.as_str().into());
        crate::cli::commands::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}
