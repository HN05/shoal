//! `--permission` on `shoal resume`. A scoped process gives only what it holds,
//! which is checked for every selected space before anything resumes.
use anyhow::{Result, ensure};

use crate::{cli::context::Context, config::permissions::PermissionChange};
use shoal_protocol::space_names::SpaceSelector;

/// `--all` names no space, so it is refused too. It writes nothing.
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
