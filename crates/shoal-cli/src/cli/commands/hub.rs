//! `shoal hub` and `shoal daemon connect`.
use anyhow::Result;

use crate::cli::{args::hub::HubCommand, context::Context};

mod members;
mod membership;
mod pairing;
mod relay;
mod updating;

pub(in crate::cli::commands) use members::role_line;
pub(in crate::cli::commands) use relay::relay;

pub(in crate::cli::commands) async fn run(ctx: &Context, command: HubCommand) -> Result<i32> {
    match command {
        HubCommand::Pair => pairing::pair(ctx).await,
        HubCommand::Join(args) => membership::join(ctx, args).await,
        HubCommand::Leave => membership::leave(ctx).await,
        HubCommand::Ls => members::list(ctx).await,
        HubCommand::Revoke { member } => members::revoke(ctx, member).await,
        HubCommand::Update => updating::update(ctx).await,
    }
}
