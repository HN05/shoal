//! The runner each repository registers on its forge: registered while
//! `runner.enabled` is on for it, with the user's own login, and deleted on
//! the forge once the setting is off or the repository is removed. On a
//! forge whose runners each take one job, registration only checks that the
//! login may register them and keeps no runner.
use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

use crate::daemon::workspace::Manager;
use crate::{
    daemon::notifications::NotificationKind,
    forge::{ForgeRepo, account::Account, ci::runner::RegisteredRunner},
    model::Repository,
};

/// A repository's runner as the daemon keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registration {
    /// The web URL of the forge repository it serves.
    pub forge_repository: String,
    /// The repository's runner, on a forge that keeps one.
    pub runner: Option<RegisteredRunner>,
}

impl Manager {
    /// Register a runner for each repository that has `runner.enabled` on
    /// and none, and delete the runner of each that turned it off. A failure
    /// leaves that repository for the next pass and tells the user once.
    pub(crate) async fn reconcile_runners(&self) -> Result<()> {
        let config = self.config();
        for repository in self.repositories().await? {
            let enabled = match self.repository_settings_from(&config, &repository.id).await {
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
                (true, None) if self.runners.due(&repository.id) => {
                    self.register_runner_unless_removed(&repository).await
                }
                (false, Some(registration)) => self.delete_runner(&repository, &registration).await,
                _ => continue,
            };
            if let Err(error) = result {
                self.runners.failed(&repository.id);
                self.notify(
                    None,
                    NotificationKind::RunnerFailed,
                    format!(
                        "The runner of {} could not change: {error:#}",
                        crate::repository::name(&repository)
                    ),
                )
                .await;
            }
        }
        Ok(())
    }

    /// Delete `repository`'s runner on the forge before the repository goes;
    /// a forge that cannot be reached leaves the runner there, which the
    /// user is told to delete.
    pub(crate) async fn delete_runner_of_removed(&self, repository: &Repository) -> Result<()> {
        let Some(registration) = self.runner_registration(&repository.id).await? else {
            return Ok(());
        };
        if let Err(error) = self.delete_runner(repository, &registration).await {
            self.notify(
                None,
                NotificationKind::RunnerFailed,
                format!(
                    "The runner of {} stays registered at {}; delete it in the repository's Actions settings: {error:#}",
                    crate::repository::name(repository),
                    registration.forge_repository
                ),
            )
            .await;
        }
        Ok(())
    }

    /// The runner `repository_id` registered, if any.
    pub(crate) async fn runner_registration(
        &self,
        repository_id: &str,
    ) -> Result<Option<Registration>> {
        let id = repository_id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT forge_repository,forge_id,uuid,token FROM runners WHERE repository_id=?1",
                        [id],
                        |row| {
                            let runner = match row.get::<_, Option<i64>>(1)? {
                                Some(id) => Some(RegisteredRunner {
                                    id: id as u64,
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
        let forge = self.checkout_forge(repository).await?;
        self.runner_program(&forge)?;
        forge.require_runner(account).await?;
        Ok(forge)
    }

    /// Where `forge`'s runner program is on the daemon's `PATH`, which each
    /// job's command inherits.
    pub(super) fn runner_program(&self, forge: &ForgeRepo) -> Result<PathBuf> {
        let program = forge.runner_program()?;
        let path = self.runners.search_path();
        crate::fsutil::find_executable(program.as_ref(), &path)
            .with_context(|| format!("{program} is not on the daemon's PATH; install it"))
    }

    /// Register a runner for `repository` unless it is being removed, under
    /// the registry gate removal holds, so removal, which deletes the
    /// runner, never misses one registered meanwhile.
    async fn register_runner_unless_removed(&self, repository: &Repository) -> Result<()> {
        let _registry = self.gates.lock_registry().await;
        if self.repository(repository.id.as_str()).await.is_err()
            || self
                .ensure_repository_available(&repository.id)
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
        let account = self.user_account();
        let forge = self.runner_forge(repository, &account).await?;
        let name = format!("shoal-{}", crate::process::machine::hostname());
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
        repository_id: &str,
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
                        runner.map(|runner| i64::try_from(runner.id)).transpose()?,
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
            let forge =
                ForgeRepo::resolve(&registration.forge_repository, &self.forge_hosts()).await?;
            forge.delete_runner(&self.user_account(), runner.id).await?;
        }
        let id = repository.id.clone();
        self.store
            .run(move |db| {
                db.execute("DELETE FROM runners WHERE repository_id=?1", [id])?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
