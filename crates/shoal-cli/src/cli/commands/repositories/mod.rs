//! Dispatch repository commands to their workflows.

mod configuration;
mod elsewhere;
mod listing;
mod moving;
mod naming;
mod registration;
mod removal;
mod selection;
mod sharing;
mod sync;

pub(super) use configuration::local_config_path;
pub(super) use registration::{offer_registration, offer_unregistered};
pub(super) use selection::{REPOSITORY, absolute, chosen};

use anyhow::Result;

use crate::cli::{args::repository::RepoCommand, context::Context};

pub(super) async fn run(ctx: &Context, command: RepoCommand) -> Result<i32> {
    match command {
        RepoCommand::Labels(repository) => super::labels::labels(ctx, repository).await,
        RepoCommand::Sync { repository } => sync::sync(ctx, repository).await,
        RepoCommand::Config { repository } => configuration::show(ctx, repository).await,
        RepoCommand::Add {
            source,
            name,
            path,
            shared,
        } => registration::add(ctx, source, name, path, shared).await,
        RepoCommand::Ls => listing::list(ctx).await,
        RepoCommand::Rename { repository, name } => naming::rename(ctx, repository, name).await,
        RepoCommand::Share { repository } => sharing::share(ctx, repository, true).await,
        RepoCommand::Unshare { repository } => sharing::share(ctx, repository, false).await,
        RepoCommand::Setup {
            repository,
            here,
            agent,
            model,
            args,
        } => {
            let options = super::repo_setup::Options {
                here,
                agent,
                model: model.into(),
                args,
            };
            super::repo_setup::start(ctx, repository, options).await
        }
        RepoCommand::Rm {
            repository,
            confirmation,
        } => removal::remove(ctx, repository, confirmation.yes).await,
        RepoCommand::Move(args) => {
            moving::run(ctx, args).await?;
            Ok(0)
        }
    }
}
