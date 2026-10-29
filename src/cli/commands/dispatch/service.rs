use anyhow::Result;

use crate::{
    cli::{
        args::service::{ConfigCommand, ServiceCommand},
        commands::{configuration, service, update},
        context::Context,
    },
    daemon::handoff::Handoff,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    command: ServiceCommand,
    handoff: Option<Handoff>,
) -> Result<i32> {
    match command {
        ServiceCommand::Config { command } => match command {
            ConfigCommand::Set {
                assignments,
                repository,
            } => configuration::edit(&ctx, configuration::sets(assignments)?, repository).await,
            ConfigCommand::Unset { keys, repository } => {
                configuration::edit(&ctx, configuration::unsets(keys), repository).await
            }
            ConfigCommand::Show { workspace } => configuration::show(&ctx, workspace).await,
            ConfigCommand::Reference => configuration::reference(&ctx),
            ConfigCommand::Reset => service::install_config(&ctx, None).await,
            ConfigCommand::Install { name } => {
                let name = service::packaged_config(&ctx, name)?;
                service::install_config(&ctx, Some(name)).await
            }
        },
        ServiceCommand::Update => update::run(&ctx).await,
        ServiceCommand::Daemon { command } => service::run(ctx, command, handoff).await,
    }
}
