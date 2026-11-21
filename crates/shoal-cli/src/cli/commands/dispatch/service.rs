use anyhow::Result;

use crate::cli::{
    args::service::{ConfigCommand, ServiceCommand},
    commands::{config_setup, configuration, hub, service, update},
    context::Context,
    entry::Daemon,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    command: ServiceCommand,
    daemon: Daemon,
) -> Result<i32> {
    match command {
        ServiceCommand::Config { command } => match command {
            ConfigCommand::Set {
                assignments,
                repository,
                shared,
            } => {
                let file = configuration::EditedFile::new(repository, shared);
                configuration::edit(&ctx, configuration::sets(assignments)?, file).await
            }
            ConfigCommand::Unset {
                keys,
                repository,
                shared,
            } => {
                let file = configuration::EditedFile::new(repository, shared);
                configuration::edit(&ctx, configuration::unsets(keys), file).await
            }
            ConfigCommand::Show {
                workspace,
                repository,
            } => configuration::show(&ctx, workspace, repository).await,
            ConfigCommand::Reference => configuration::reference(&ctx),
            ConfigCommand::Reload => configuration::reload(&ctx).await,
            ConfigCommand::Reset => service::install_config(&ctx, None).await,
            ConfigCommand::Install { name } => {
                let name = service::packaged_config(&ctx, name)?;
                service::install_config(&ctx, Some(name)).await
            }
            ConfigCommand::Setup(args) => config_setup::start(&ctx, *args).await,
        },
        ServiceCommand::Update { version } => update::run(&ctx, version).await,
        ServiceCommand::Daemon { command } => service::run(ctx, command, daemon).await,
        ServiceCommand::Hub { command } => hub::run(&ctx, command).await,
    }
}
