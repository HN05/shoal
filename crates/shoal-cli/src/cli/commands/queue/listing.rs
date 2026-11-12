//! The `shoal queue` listing.
use anyhow::Result;

use crate::cli::{
    commands::{listing::ListFilter, queued_agents},
    context::Context,
};

/// List the entries of `repository`, of every repository without one,
/// each led by the ID that `shoal queue start` and `cancel` take.
pub(super) async fn list(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let filter = ListFilter::resolve(ctx, repository, Vec::new()).await?;
    let entries = queued_agents::listed(ctx, &filter).await?;
    let rows = queued_agents::rows(ctx, &entries).await?;
    ctx.show(&entries, |_| {
        if rows.is_empty() {
            outln!("No queued agents");
        }
        for (entry, row) in entries.iter().zip(rows) {
            outln!("{}  {row}", entry.id);
        }
    })?;
    Ok(0)
}
