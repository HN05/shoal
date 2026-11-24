//! The runner each repository registers on its forge: registered while
//! `runner.enabled` is on for it, with the user's own login, and deleted on
//! the forge once the setting is off or the repository is removed. On a
//! forge whose runners each take one job, registration only checks that the
//! login may register them and keeps no runner.
use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use shoal_core::fsutil::executables::find_executable;
use shoal_daemon_base::alerts::{About, NewAlert};
use shoal_forge::{
    ci::runner::{RegisteredRunner, RunnerRequests},
    transport::account::Account,
};
use shoal_forge_types::ForgeRepo;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{alerts::AlertKind, model::Repository};

use crate::{Runner, daemon::Daemon};

/// A repository's runner as the daemon keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The web URL of the forge repository it serves.
    pub forge_repository: String,
    /// The repository's runner, on a forge that keeps one.
    pub runner: Option<RegisteredRunner>,
}

impl Runner {
    /// Register a runner for each repository that has `runner.enabled` on
    /// and none, and delete the runner of each that turned it off. A failure
    /// leaves that repository for the next pass and tells the user once.
    pub async fn reconcile_runners(&self, daemon: &dyn Daemon) -> Result<()> {
        let config = self.config();
        for repository in daemon.repositories().await? {
            let settings = daemon.repository_settings_from(&config, &repository.id);
            let enabled = match settings.await {
                Ok(settings) => settings.runner.enabled,
                // A config that does not resolve leaves the runner as it is.
                Err(_) => continue,
            };
            if !enabled {
                // Its waiting jobs stay with the forge for other runners.
                self.forget_job_entries(&repository.id).await?;
            }
            let registered = self.runner_registration(&repository.id).await?;
            let result = match (enabled, registered) {
                (true, None) if self.jobs.due(&repository.id) => {
                    self.register_runner_unless_removed(daemon, &repository)
                        .await
                }
                (false, Some(registration)) => self.delete_runner(&repository, &registration).await,
                _ => continue,
            };
            if let Err(error) = result {
                self.jobs.failed(&repository.id);
                let message = format!(
                    "The runner of {} could not change: {error:#}",
                    repository.display_name()
                );
                self.notify_runner_failed(message).await;
            }
        }
        Ok(())
    }

    /// Delete `repository`'s runner on the forge before the repository goes;
    /// a forge that cannot be reached leaves the runner there, which the
    /// user is told to delete.
    pub async fn delete_runner_of_removed(&self, repository: &Repository) -> Result<()> {
        let Some(registration) = self.runner_registration(&repository.id).await? else {
            return Ok(());
        };
        if let Err(error) = self.delete_runner(repository, &registration).await {
            let message = format!(
                "The runner of {} stays registered at {}; delete it in the repository's Actions settings: {error:#}",
                repository.display_name(),
                registration.forge_repository
            );
            self.notify_runner_failed(message).await;
        }
        Ok(())
    }

    /// The runner `repository_id` registered, if any.
    pub async fn runner_registration(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<Option<Registration>> {
        let id = repository_id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT forge_repository,forge_id,uuid,token FROM runners WHERE repository_id=?1",
                        [id],
                        |row| {
                            let runner = match row.get(1)? {
                                Some(id) => Some(RegisteredRunner {
                                    id,
                                    uuid: row.get(2)?,
                                    token: row.get(3)?,
                                }),
                                None => None,
                            };
                            Ok(Registration {
                                forge_repository: row.get(0)?,
                                runner,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    /// The forge of `repository`'s checkout, refused unless this machine
    /// can run its jobs as `account`: the forge supports it and its runner
    /// program is on the daemon's `PATH`.
    async fn runner_forge(&self, repository: &Repository, account: &Account) -> Result<ForgeRepo> {
        let forge = self.forges.checkout_forge(repository).await?;
        self.runner_program(&forge)?;
        forge.require_runner(account).await?;
        Ok(forge)
    }

    /// Where `forge`'s runner program is on the daemon's `PATH`, which each
    /// job's command inherits.
    pub fn runner_program(&self, forge: &ForgeRepo) -> Result<PathBuf> {
        let program = forge.runner_program()?;
        let path = self.jobs.search_path();
        find_executable(program.as_ref(), &path)
            .with_context(|| format!("{program} is not on the daemon's PATH; install it"))
    }

    /// Register a runner for `repository` unless it is being removed, under
    /// the registry gate removal holds, so removal, which deletes the
    /// runner, never misses one registered meanwhile.
    async fn register_runner_unless_removed(
        &self,
        daemon: &dyn Daemon,
        repository: &Repository,
    ) -> Result<()> {
        let _registry = self.gates.lock_registry().await;
        if daemon.repository(&repository.id).await.is_err()
            || (daemon.ensure_repository_available(&repository.id))
                .await
                .is_err()
        {
            return Ok(());
        }
        self.register_runner(repository).await
    }

    /// Register a runner for `repository` on its forge and keep it.
    async fn register_runner(&self, repository: &Repository) -> Result<()> {
        // One account, so a token from a command is fetched once.
        let account = self.forges.user_account();
        let forge = self.runner_forge(repository, &account).await?;
        let name = format!("shoal-{}", shoal_core::process::machine::hostname());
        let registration = Registration {
            forge_repository: forge.repository_url(),
            runner: forge.register_runner(&account, &name).await?,
        };
        let forge_id = registration.runner.as_ref().map(|runner| runner.id);
        if let Err(error) = self.keep_registration(&repository.id, registration).await {
            // Unkept, the runner would stay on the forge with nothing to delete it.
            if let Some(id) = forge_id {
                let _ = forge.delete_runner(&account, id).await;
            }
            return Err(error);
        }
        Ok(())
    }

    async fn keep_registration(
        &self,
        repository_id: &RepositoryId,
        registration: Registration,
    ) -> Result<()> {
        let id = repository_id.to_owned();
        self.store
            .run(move |db| {
                let runner = registration.runner.as_ref();
                db.execute(
                    "INSERT INTO runners(repository_id,forge_repository,forge_id,uuid,token)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        id,
                        registration.forge_repository,
                        runner.map(|runner| runner.id),
                        runner.map(|runner| &runner.uuid),
                        runner.map(|runner| &runner.token)
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// Delete `registration`, `repository`'s runner, on the forge it serves
    /// and forget it.
    async fn delete_runner(
        &self,
        repository: &Repository,
        registration: &Registration,
    ) -> Result<()> {
        if let Some(runner) = &registration.runner {
            let forge = self.forges.forge(&registration.forge_repository).await?;
            forge
                .delete_runner(&self.forges.user_account(), runner.id)
                .await?;
        }
        let id = repository.id.clone();
        self.store
            .run(move |db| {
                db.execute("DELETE FROM runners WHERE repository_id=?1", [id])?;
                Ok(())
            })
            .await
    }

    /// Tell the user `message` about a runner that failed.
    async fn notify_runner_failed(&self, message: String) {
        let kind = AlertKind::RunnerFailed;
        let alert = NewAlert::shoal(kind, About::default(), message);
        self.notifications.notify_user(alert).await;
    }
}

#[cfg(test)]
mod tests;
