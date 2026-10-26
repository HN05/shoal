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
    /// The proxy the agent's API requests go through, which runs as long as
    /// this wrapper does.
    pub(super) proxy: Option<crate::proxy::Running>,
}

impl AgentLaunch {
    /// `command` with its proxy placeholders filled in, for an agent that
    /// runs with the workspace's `environment` and its own.
    fn route(
        &self,
        command: &[OsString],
        environment: &std::collections::BTreeMap<String, OsString>,
    ) -> Result<Vec<OsString>> {
        let Some(proxy) = &self.proxy else {
            return Ok(command.to_vec());
        };
        let mut configured = environment.clone();
        self.environment.layer(&mut configured, &self.home);
        crate::cli::agents::route_through_proxy(
            self.integration.unwrap_or(crate::ai::IntegrationKind::None),
            command.to_vec(),
            &proxy.url,
            &self.home,
            &configured,
        )
    }

    fn apply(&self, command: &mut Command) {
        self.account.apply(command);
        self.secrets.apply(command);
        command.envs(self.output);
        self.environment.apply(command.as_std_mut(), &self.home);
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
        Some(auth) => auth.route(command, &plan.environment)?,
        None => command.to_vec(),
    };
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(&plan.workspace.path);
    configure_environment(&mut process, paths, plan);
    if let Some(auth) = auth {
        auth.apply(&mut process);
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
