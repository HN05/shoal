//! The `shoal queue` listing.
use anyhow::Result;

use crate::cli::{
    commands::{agent_queue, listing::ListFilter},
    context::Context,
};

/// List the entries of `repository`, of every repository without one.
pub(super) async fn list(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let filter = ListFilter::resolve(ctx, repository, Vec::new()).await?;
    let entries = agent_queue::listed(ctx, &filter).await?;
    let rows = agent_queue::rows(ctx, &entries).await?;
    ctx.show(&entries, |_| {
        if rows.is_empty() {
            outln!("No queued agents");
        }
        for row in rows {
            outln!("{row}");
        }
    })?;
    Ok(0)
}
