//! The daemon's checks of its environment: the tools on its `PATH`,
//! `root_dir`, registered checkouts, the global config and the forges
//! recorded hosts serve. It reports the
//! problems they find as alerts when it starts and when its config reloads.
use std::{ffi::OsStr, path::PathBuf};

use anyhow::Result;

use crate::{
    daemon::{log, notifications::NotificationKind, workspace::Manager},
    tools::{Dependency, Tool},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CheckStatus {
    Ok,
    Warning,
    Error,
}

/// One finding of a check: whether and why it failed.
pub(super) struct Check {
    pub(super) status: CheckStatus,
    pub(super) message: String,
}

impl Check {
    pub(super) fn new(status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

fn dependencies(path: Option<&OsStr>) -> Vec<Check> {
    Tool::dependencies()
        .map(|(tool, dependency)| {
            let program = tool.program();
            let executable = path.and_then(|path| crate::fsutil::find_executable(OsStr::new(program), path));
            let (status, message) = match executable {
                Some(path) => (CheckStatus::Ok, format!("{} on the daemon's PATH", path.display())),
                None => (
                    match dependency { Dependency::Required => CheckStatus::Error, Dependency::Optional(_) => CheckStatus::Warning },
                    format!(
                        "{program} is missing or not executable on the daemon's PATH{}; install it and restart the daemon with the updated PATH",
                        match dependency { Dependency::Required => String::new(), Dependency::Optional(reason) => format!(" ({reason})") }
                    ),
                ),
            };
            Check::new(status, message)
        })
        .collect()
}

/// Whether Git can use the working tree of a registered checkout.
async fn checkout(repo: &crate::model::Repository) -> Check {
    match crate::repository::bare_flag_problem(&repo.path).await {
        Ok(None) => Check::new(CheckStatus::Ok, "Git config does not set core.bare"),
        Ok(Some(problem)) => Check::new(
            CheckStatus::Warning,
            format!("{problem}, then check git status: the index may lag behind HEAD"),
        ),
        Err(error) => Check::new(
            CheckStatus::Error,
            format!("Could not check {}: {error:#}", repo.path.display()),
        ),
    }
}

/// Whether the daemon can create files in `root_dir`, where new repositories
/// and their workspaces go, resolved as registration resolves it.
async fn root_dir(root: Result<PathBuf>) -> Check {
    let root =
        root.and_then(|root| crate::daemon::workspace::paths::canonical_with_missing_tail(&root));
    let checked = match root {
        Ok(root) => crate::daemon::access::ensure_writable(&root)
            .await
            .map(|()| root),
        Err(error) => Err(error),
    };
    match checked {
        Ok(root) => Check::new(
            CheckStatus::Ok,
            format!("The daemon can write to {}", root.display()),
        ),
        Err(error) => Check::new(
            CheckStatus::Error,
            format!("Cannot use root_dir: {error:#}"),
        ),
    }
}

/// Whether the global config sets something Shoal no longer reads or already
/// says; `config show` reports the repository layers too.
fn global_config_check(paths: &crate::paths::Paths) -> Check {
    let retired = crate::config::Config::load_with_templates(paths)
        .map(|config| config.repository_layer().normalize());
    match retired {
        Ok(retired) if retired.is_empty() => Check::new(
            CheckStatus::Ok,
            "The global config sets no retired or redundant settings".to_owned(),
        ),
        Ok(retired) => Check::new(CheckStatus::Warning, retired.join("; ")),
        Err(error) => Check::new(
            CheckStatus::Error,
            format!("Could not read the global config: {error:#}"),
        ),
    }
}

/// Check the environment once the daemon has started and again after each
/// config reload, outside any request so slow checks delay no reply.
pub async fn watch_environment(manager: std::sync::Arc<Manager>) {
    loop {
        {
            let _operation = manager.background_operations.read().await;
            manager.report_environment().await;
        }
        manager.environment_recheck.notified().await;
    }
}

impl Manager {
    /// Record an alert for each problem with the daemon's tools, `root_dir`,
    /// registered checkouts and the global config, and for each recorded host
    /// that now serves another forge, as the daemon starts or reloads its
    /// config. A missing optional tool only disables what needs it, so it
    /// alerts nobody. Identical unread alerts collapse into one.
    pub async fn report_environment(&self) {
        let mut checks: Vec<_> = dependencies(std::env::var_os("PATH").as_deref())
            .into_iter()
            .filter(|check| check.status == CheckStatus::Error)
            .collect();
        checks.push(root_dir(self.config().root_dir(&self.paths)).await);
        checks.push(global_config_check(&self.paths));
        match self.repositories().await {
            Ok(repositories) => {
                for repo in repositories {
                    checks.push(checkout(&repo).await);
                }
            }
            Err(error) => log!("environment check skipped repositories: {error:#}"),
        }
        let mut problems: Vec<String> = checks
            .into_iter()
            .filter(|check| check.status != CheckStatus::Ok)
            .map(|check| check.message)
            .collect();
        // Only a changed answer matters: a host Shoal cannot reach now keeps
        // the forge it answered before, which a machine offline at boot needs.
        match self.recheck_forge_hosts().await {
            Ok(rechecks) => {
                problems.extend(rechecks.iter().map(|(host, change)| change.message(host)))
            }
            Err(error) => log!("environment check skipped forge hosts: {error:#}"),
        }
        for problem in problems {
            self.notify(None, NotificationKind::EnvironmentProblem, problem)
                .await;
        }
    }
}

#[cfg(test)]
mod tests;
