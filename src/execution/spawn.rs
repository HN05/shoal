//! Spawning a tracked command in its workspace, with the environment, agent
//! account and output streams its mode gives it.
use std::{ffi::OsString, path::PathBuf, process::Stdio};

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

use super::Mode;
use crate::{env, model::ExecutionPlan, paths::Paths};

/// What an agent's or publishing child receives beyond the workspace
/// environment: the agent account and, for agents, the configured secrets and
/// the output default.
pub(super) struct AgentLaunch {
    pub(super) account: crate::agent_auth::Launch,
    pub(super) secrets: crate::secrets::Launch,
    pub(super) output: Option<(&'static str, &'static str)>,
    /// The agent's integration, which launch profiles share with their base.
    pub(super) integration: Option<crate::ai::IntegrationKind>,
    /// What the agent's `[ai]` entry changes in its environment, last.
    pub(super) environment: crate::ai::Environment,
    pub(super) home: PathBuf,
    /// The proxy the agent's API requests go through, which
    /// [`AgentLaunch::start_proxy`] runs as long as this wrapper does.
    pub(super) routed: Option<crate::proxy::Proxy>,
    pub(super) proxy: Option<crate::proxy::Running>,
}

impl AgentLaunch {
    /// Start the agent's proxy, which may take its key from the environment
    /// the agent runs with.
    pub(super) async fn start_proxy(
        &mut self,
        environment: &std::collections::BTreeMap<String, OsString>,
    ) -> Result<()> {
        let Some(routed) = &self.routed else {
            return Ok(());
        };
        let configured = self.configured_environment(environment);
        let variable = |name: &str| self.variable(&configured, name);
        self.proxy = Some(crate::proxy::start(routed, variable).await?);
        Ok(())
    }

    /// Run the restored agent, with the workspace's `environment`, through
    /// `proxy`, which its restore command was resolved with: the one running
    /// when it is the same, otherwise a new one, or none. Whether it changed,
    /// so what the old one reported no longer holds.
    pub(super) async fn restart_proxy(
        &mut self,
        proxy: Option<crate::proxy::Proxy>,
        environment: &std::collections::BTreeMap<String, OsString>,
    ) -> Result<bool> {
        if self.routed == proxy {
            return Ok(false);
        }
        self.routed = proxy;
        self.proxy = None;
        self.start_proxy(environment).await?;
        Ok(true)
    }

    /// Have the agent's proxy, when one runs, report to the daemon for
    /// `execution` in `workspace`, its turns' ends when the proxy's failure
    /// stream ends them.
    pub(super) fn report_proxy(&mut self, paths: &Paths, workspace: &str, execution: &str) {
        let kind = self.integration.unwrap_or(crate::ai::IntegrationKind::None);
        let ends_turns = crate::cli::agents::proxy_ends_turns(kind);
        if let Some(proxy) = self.proxy.as_mut() {
            proxy.report_turns(paths, workspace, execution, ends_turns);
        }
    }

    /// What the agent's launch adds to this process's environment: the
    /// workspace's `environment`, the secrets, then the agent's own.
    fn configured_environment(
        &self,
        environment: &std::collections::BTreeMap<String, OsString>,
    ) -> std::collections::BTreeMap<String, OsString> {
        let mut configured = environment.clone();
        configured.extend(self.secrets.environment());
        self.environment.layer(&mut configured, &self.home);
        configured
    }

    /// Variable `name` as the agent sees it: as `configured` sets it, else
    /// as this process has it unless the agent's own environment removes it.
    fn variable(
        &self,
        configured: &std::collections::BTreeMap<String, OsString>,
        name: &str,
    ) -> Option<OsString> {
        match configured.get(name) {
            Some(value) => Some(value.clone()),
            None if self.environment.remove.contains(name) => None,
            None => std::env::var_os(name),
        }
    }

    /// `command` with its proxy placeholders filled in, for an agent that
    /// runs with the workspace's `environment` and its own.
    fn route(
        &self,
        command: &[OsString],
        environment: &std::collections::BTreeMap<String, OsString>,
        directory: &std::path::Path,
    ) -> Result<Vec<OsString>> {
        let Some(proxy) = &self.proxy else {
            return Ok(command.to_vec());
        };
        let mut configured = environment.clone();
        self.environment.layer(&mut configured, &self.home);
        let routing = crate::cli::agents::Routing {
            url: &proxy.url,
            directory,
            private: proxy.private_dir(),
        };
        crate::cli::agents::route_through_proxy(
            self.integration.unwrap_or(crate::ai::IntegrationKind::None),
            command.to_vec(),
            &routing,
            &self.home,
            &configured,
        )
    }

    /// Give `command` the agent's environment, over the workspace's
    /// `environment`, and the key its proxy wants, when one runs.
    fn apply(
        &self,
        command: &mut Command,
        environment: &std::collections::BTreeMap<String, OsString>,
    ) {
        self.account.apply(command);
        self.secrets.apply(command);
        command.envs(self.output);
        self.environment.apply(command.as_std_mut(), &self.home);
        if let Some(proxy) = &self.proxy {
            command.envs(self.proxy_environment(&proxy.key, environment));
        }
    }

    /// What the agent's environment gets so its requests name its proxy's
    /// `key`, joining what it has, such as its own extra headers.
    fn proxy_environment(
        &self,
        key: &str,
        environment: &std::collections::BTreeMap<String, OsString>,
    ) -> Vec<(&'static str, OsString)> {
        let configured = self.configured_environment(environment);
        let kind = self.integration.unwrap_or(crate::ai::IntegrationKind::None);
        let variable = |name: &str| self.variable(&configured, name);
        crate::cli::agents::proxy_environment(kind, key, &variable)
    }
}

pub(super) fn spawn(
    paths: &Paths,
    plan: &ExecutionPlan,
    command: &[OsString],
    mode: &Mode,
    auth: Option<&AgentLaunch>,
) -> Result<Child> {
    let command = match auth {
        Some(auth) => auth.route(command, &plan.environment, &plan.workspace.path)?,
        None => command.to_vec(),
    };
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(&plan.workspace.path);
    configure_environment(&mut process, paths, plan);
    if let Some(auth) = auth {
        auth.apply(&mut process, &plan.environment);
    }
    export_execution(&mut process, paths, plan);
    // A demo is the user's, so the agent's output setting stays behind, and
    // its own port or device wins over a reservation exported as `PORT`.
    if let Mode::Demo { launch } = mode {
        process.env_remove(env::OUTPUT).envs(&launch.environment);
    }
    let quiet = matches!(mode, Mode::Setup { json: true });
    let (stdin, stdout, stderr) = match mode {
        Mode::Detached { log } => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .with_context(|| format!("open {}", log.display()))?;
            let copy = file.try_clone()?;
            (Stdio::null(), Stdio::from(file), Stdio::from(copy))
        }
        Mode::Publish => (
            Stdio::inherit(),
            Stdio::from(std::io::stderr()),
            Stdio::inherit(),
        ),
        _ if quiet => (
            Stdio::null(),
            Stdio::from(std::io::stderr()),
            Stdio::inherit(),
        ),
        _ => (Stdio::inherit(), Stdio::inherit(), Stdio::inherit()),
    };
    process
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("launch workspace command")
}

/// Export the workspace's identity and `[env]`, dropping stale port variables
/// inherited from an enclosing execution.
fn configure_environment(process: &mut Command, paths: &Paths, plan: &ExecutionPlan) {
    env::apply_workspace_identity(process, &plan.workspace, paths);
    env::apply_configured(process, &plan.environment);
}

/// Export the execution's scope and port reservations last, so no configured
/// variable replaces or removes them.
fn export_execution(process: &mut Command, paths: &Paths, plan: &ExecutionPlan) {
    process
        .envs(env::workspace_environment(
            &plan.workspace,
            paths,
            &plan.ports,
            &plan.scope_token,
        ))
        .env(env::EXECUTION_ID, &plan.id);
}

// Its tests run the proxy, which only that feature builds.
#[cfg(all(test, feature = "proxy"))]
mod tests;
