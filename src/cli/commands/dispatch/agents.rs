use anyhow::Result;

use crate::{
    agent::AgentMode,
    ai::IntegrationKind,
    cli::{
        agents,
        args::agents::{AgentsCommand, ChatCommand},
        commands::{chat, plan},
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
            command: Some(ChatCommand::Rename { chat, name }),
            ..
        } => chat::rename(&ctx, chat, name).await,
        AgentsCommand::Chat {
            command: None,
            here,
            repository,
            agent,
            name,
            model,
            args,
        } => {
            let options = chat::Options {
                agent,
                here,
                name,
                model: model.into(),
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
            model,
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
                model: model.into(),
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
