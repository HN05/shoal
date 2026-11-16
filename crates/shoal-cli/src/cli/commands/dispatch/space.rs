use anyhow::Result;

use crate::cli::{args::space::SpaceCommand, commands::spaces, context::Context};

pub(in crate::cli::commands) async fn run(ctx: Context, command: SpaceCommand) -> Result<i32> {
    match command {
        SpaceCommand::Open { space, app, args } => spaces::open(&ctx, space, app, args).await,
    }
}
