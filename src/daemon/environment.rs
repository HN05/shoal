//! The daemon's checks of its environment: the tools on its `PATH`,
//! `root_dir`, registered checkouts, the global config and the forges
//! recorded hosts serve. It reports the
//! problems they find as alerts when it starts and when its config reloads.
use std::{ffi::OsStr, path::PathBuf};

use anyhow::Result;

use crate::{
    config::repo::DefaultHost,
    daemon::{log, notifications::NotificationKind, workspace::Manager},
    sessions::HostName,
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

fn dependencies(path: Option<&OsStr>, default_host: HostName) -> Vec<Check> {
    Tool::dependencies()
        .filter_map(|(tool, dependency)| {
            let (status, reason) = when_missing(dependency, default_host)?;
            let program = tool.program();
            let executable = path.and_then(|path| crate::fsutil::find_executable(OsStr::new(program), path));
            Some(match executable {
                Some(path) => Check::new(CheckStatus::Ok, format!("{} on the daemon's PATH", path.display())),
                None => Check::new(
                    status,
                    format!("{program} is missing or not executable on the daemon's PATH{reason}; install it and restart the daemon with the updated PATH"),
                ),
            })
        })
        .collect()
}

/// How `dependency` fails the check when missing, with why Shoal needs it;
/// none for the program of a terminal host other than `default_host`.
fn when_missing(dependency: Dependency, default_host: HostName) -> Option<(CheckStatus, String)> {
    match dependency {
        Dependency::Required => Some((CheckStatus::Error, String::new())),
        Dependency::Optional(reason) => Some((CheckStatus::Warning, format!(" ({reason})"))),
        Dependency::Host(host) if host == default_host => Some((
            CheckStatus::Error,
            " (the default terminal host; terminal.default_host can name another)".to_owned(),
        )),
        Dependency::Host(_) => None,
    }
}

/// The default terminal host the global config sets, which a config that
/// does not load leaves at its default; its own check reports that.
fn default_host(config: &crate::config::Config) -> HostName {
    config
        .resolve(&crate::config::repo::ConfigLayers::default())
        .map_or_else(
            |_| DefaultHost::default().into(),
            |settings| settings.terminal.default_host,
        )
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

/// Whether the global config repeats what Shoal already says; `config show`
/// reports the repository layers too.
fn global_config_check(paths: &crate::paths::Paths) -> Check {
    let redundant = crate::config::Config::load_with_templates(paths)
        .map(|config| config.repository_layer().normalize());
    match redundant {
        Ok(redundant) if redundant.is_empty() => Check::new(
            CheckStatus::Ok,
            "The global config sets no redundant settings".to_owned(),
        ),
        Ok(redundant) => Check::new(CheckStatus::Warning, redundant.join("; ")),
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
            let _operation = manager.gates.operation().await;
            manager.report_environment().await;
        }
        manager.signals.environment_recheck().await;
    }
}

impl Manager {
    /// Reload the global config, then recheck the environment with it in
    /// [`watch_environment`], since the checks can outlast a reply's budget.
    pub async fn reload_config_and_recheck(&self) -> Result<()> {
        self.reload_config().await?;
        self.signals.recheck_environment();
        Ok(())
    }

    /// Record an alert for each problem with the daemon's tools, the default
    /// terminal host's among them, `root_dir`, registered checkouts and the
    /// global config, and for each recorded host that now serves another
    /// forge, as the daemon starts or reloads its config. A missing optional tool only disables what needs it, so it
    /// alerts nobody. Identical unread alerts collapse into one.
    pub async fn report_environment(&self) {
        let default_host = default_host(&self.config());
        let mut checks: Vec<_> = dependencies(std::env::var_os("PATH").as_deref(), default_host)
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
