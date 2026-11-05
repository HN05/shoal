use anyhow::Result;

use crate::cli::{agents, args::agents::AgentsCommand, context::Context};

pub(in crate::cli::commands) async fn run(ctx: Context, command: AgentsCommand) -> Result<i32> {
    match command {
        #[cfg(feature = "happy")]
        AgentsCommand::Happy {
            agent,
            workspace,
            prompt,
            args,
        } => agents::happy(&ctx, agent, workspace, prompt, args).await,
    }
}
