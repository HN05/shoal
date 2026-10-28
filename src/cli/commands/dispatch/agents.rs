use anyhow::Result;

use crate::{
    agent::AgentMode,
    ai::IntegrationKind,
    cli::{
        agents,
        args::agents::AgentsCommand,
        commands::{chat, plan},
        context::Context,
    },
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: AgentsCommand) -> Result<i32> {
    match command {
        AgentsCommand::Claude { workspace, args } => {
            agents::run_named(&ctx, "claude", workspace, args).await
        }
        AgentsCommand::Codex {
            workspace,
            cli,
            app,
            args,
        } => {
            let mode = mode(cli, app);
            agents::with_app(&ctx, IntegrationKind::Codex, mode, workspace, args).await
        }
        AgentsCommand::T3 { workspace, args } => {
            agents::open_app(&ctx, workspace, &agents::T3, args).await
        }
        #[cfg(feature = "happy")]
        AgentsCommand::Happy {
            agent,
            workspace,
            prompt,
            args,
        } => agents::happy(&ctx, agent, workspace, prompt, args).await,
        AgentsCommand::Chat {
            here,
            repository,
            agent,
            name,
            args,
        } => {
            let options = chat::Options {
                agent,
                here,
                name,
                args,
            };
            chat::start(&ctx, repository, options).await
        }
        AgentsCommand::Plan {
            issue,
            oldest,
            newest,
            repository,
            here,
            agent,
            name,
            args,
        } => {
            let pick = match (oldest, newest) {
                (true, _) => plan::Pick::Oldest,
                (_, true) => plan::Pick::Newest,
                _ => plan::Pick::Picker,
            };
            let options = chat::Options {
                agent,
                here,
                name,
                args,
            };
            plan::start(&ctx, issue, repository, pick, options).await
        }
    }
}

/// The mode `--cli` or `--app` selects; neither keeps the configured one.
fn mode(cli: bool, app: bool) -> Option<AgentMode> {
    if cli {
        Some(AgentMode::Cli)
    } else if app {
        Some(AgentMode::App)
    } else {
        None
    }
}
