//! The `shoal queue` listing.
use anyhow::Result;

use crate::cli::{
    commands::{listing::ListFilter, queued_agents},
    context::Context,
};

/// List the entries of `repository`, of every repository without one, as a
/// table.
pub(super) async fn list(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let filter = ListFilter::resolve(ctx, repository, Vec::new()).await?;
    let entries = queued_agents::listed(ctx, &filter).await?;
    let table = queued_agents::table(ctx, &entries).await?;
    ctx.show(&entries, |_| {
        if table.rows.is_empty() {
            outln!("No queued agents");
            return;
        }
        outln!("{}", table.header);
        for row in table.rows {
            outln!("{row}");
        }
    })?;
    Ok(0)
}
