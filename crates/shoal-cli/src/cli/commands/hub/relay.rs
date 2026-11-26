//! `shoal daemon connect`: the relay a joined machine reaches the hub's daemon
//! by.
use anyhow::Result;

use crate::cli::context::Context;

pub(in crate::cli::commands) async fn relay(ctx: &Context) -> Result<i32> {
    let (input, output) = (tokio::io::stdin(), tokio::io::stdout());
    crate::client::hub::relay(&ctx.paths, input, output).await?;
    Ok(0)
}
