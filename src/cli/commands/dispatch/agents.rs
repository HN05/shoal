use anyhow::Result;

use crate::{
    agent::AgentMode,
    ai::IntegrationKind,
    cli::{
        agents,
        args::{agents::AgentsCommand, workspace::DesktopApp},
        context::Context,
    },
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: AgentsCommand) -> Result<i32> {
    match command {
        AgentsCommand::Claude {
            workspace,
            cli,
            app,
            model,
            args,
        } => {
            let mode = mode(cli, app);
            let kind = IntegrationKind::Claude;
            agents::with_app(&ctx, kind, mode, workspace, model.into(), args).await
        }
        AgentsCommand::Codex {
            workspace,
            cli,
            app,
            model,
            args,
        } => {
            let mode = mode(cli, app);
            let kind = IntegrationKind::Codex;
            agents::with_app(&ctx, kind, mode, workspace, model.into(), args).await
        }
        AgentsCommand::T3 { workspace, args } => {
            agents::open(&ctx, workspace, agents::app(DesktopApp::T3), args).await
        }
        #[cfg(feature = "happy")]
        AgentsCommand::Happy {
            agent,
            workspace,
            prompt,
            args,
        } => agents::happy(&ctx, agent, workspace, prompt, args).await,
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
