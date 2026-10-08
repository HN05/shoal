//! Environment variables Shoal reads or exports. Every `SHOAL_*` name lives
//! here so wrapper, daemon, and process scanning agree on the contract.
use anyhow::{Result, ensure};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};
use tokio::process::Command;

use crate::{
    model::{PortReservation, Workspace},
    paths::Paths,
};

pub const PREFIX: &str = "SHOAL_";

/// Overrides the state directory; also isolates the daemon socket.
pub const STATE_DIR: &str = "SHOAL_STATE_DIR";
/// Resolved launch plan a new Herdr tab hands its worker; removed at startup.
pub const HERDR_PLAN: &str = "SHOAL_HERDR_PLAN";
/// Cooperative workspace scope for tracked or externally launched processes.
pub const SCOPE_TOKEN: &str = "SHOAL_SCOPE_TOKEN";
/// Execution marker used to discover owned processes during recovery.
pub const EXECUTION_ID: &str = "SHOAL_EXECUTION_ID";
pub const WORKSPACE_ID: &str = "SHOAL_WORKSPACE_ID";
/// Legacy alias of [`WORKSPACE_ID`] kept for existing agent integrations.
pub const RUN_ID: &str = "SHOAL_RUN_ID";
pub const WORKSPACE_NAME: &str = "SHOAL_WORKSPACE";
/// Colon-separated names of the port variables exported to the command.
pub const RESERVED_PORT_ENV: &str = "SHOAL_RESERVED_PORT_ENV";
/// Prefix of the default environment variable for a named port reservation.
pub const PORT_PREFIX: &str = "SHOAL_PORT_";
/// Which hook is running, using the configuration key without `_cmd`.
pub const HOOK: &str = "SHOAL_HOOK";
/// Recorded worktree path, including after removal.
pub const WORKSPACE_PATH: &str = "SHOAL_WORKSPACE_PATH";
/// JSON resource lease passed to permit hooks.
pub const RESOURCE_LEASE: &str = "SHOAL_RESOURCE_LEASE";
/// Requested completion policy passed to post-done hooks: `keep` or `cleanup`.
pub const DONE_CHOICE: &str = "SHOAL_DONE_CHOICE";
/// Shortcut or configured agent name passed to agent-exit hooks.
pub const AGENT: &str = "SHOAL_AGENT";
/// Reported exit code, empty when the agent disconnected without reporting.
pub const AGENT_EXIT_CODE: &str = "SHOAL_AGENT_EXIT_CODE";
/// Whether the tracked execution finished without surviving processes.
pub const AGENT_EXIT_COMPLETE: &str = "SHOAL_AGENT_EXIT_COMPLETE";
/// File the shell wrapper reads to change directory after the command exits.
pub const SHELL_DIRECTIVE: &str = "SHOAL_SHELL_DIRECTIVE";
/// The shell wrapper's `OLDPWD`, passed explicitly because it is shell-local.
pub const PREVIOUS_DIR: &str = "SHOAL_PREVIOUS_DIR";
/// clap dynamic-completion trigger variable.
pub const COMPLETE: &str = "SHOAL_COMPLETE";
/// Runtime override for the packaged skills directory.
pub const SKILLS_DIR: &str = "SHOAL_SKILLS_DIR";
/// Build-time fallback for the packaged skills directory; the macro requires a
/// literal. Named apart from the runtime override, which launched agents
/// inherit, so their own builds never embed it.
pub const COMPILED_SKILLS_DIR: Option<&str> = option_env!("SHOAL_BUILD_SKILLS_DIR");
/// Claude Code's configuration directory override (its `.claude.json` and skills).
pub const CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
/// Codex's user configuration and state directory override.
pub const CODEX_HOME: &str = "CODEX_HOME";

pub fn codex_home() -> Result<Option<PathBuf>> {
    absolute_dir_var(CODEX_HOME)
}

/// The configured Claude Code directory, if any; a relative override is an error.
pub fn claude_config_dir() -> Result<Option<PathBuf>> {
    absolute_dir_var(CLAUDE_CONFIG_DIR)
}

fn absolute_dir_var(name: &str) -> Result<Option<PathBuf>> {
    let Some(dir) = std::env::var_os(name) else {
        return Ok(None);
    };
    let dir = PathBuf::from(dir);
    ensure!(dir.is_absolute(), "{name} must be an absolute path");
    Ok(Some(dir))
}

/// Variables a reserved port may never shadow when exported to a command.
pub const PROTECTED: [&str; 4] = ["HOME", "PATH", "SHELL", "TMPDIR"];

/// Whether an inherited variable named in [`RESERVED_PORT_ENV`] may be dropped
/// before launching a command: never the protected set or Shoal's own
/// variables other than port exports.
pub fn is_port_export(name: &str) -> bool {
    !name.is_empty()
        && !PROTECTED.contains(&name)
        && (!name.starts_with(PREFIX) || name.starts_with(PORT_PREFIX))
}

/// Port variables inherited from an enclosing execution.
pub fn inherited_port_exports() -> Vec<OsString> {
    let mut exports: Vec<_> = std::env::vars_os()
        .filter_map(|(name, _)| {
            name.to_str()
                .is_some_and(|name| name.starts_with(PORT_PREFIX))
                .then_some(name)
        })
        .collect();
    if let Ok(names) = std::env::var(RESERVED_PORT_ENV) {
        exports.extend(
            names
                .split(':')
                .filter(|name| is_port_export(name))
                .map(OsString::from),
        );
    }
    exports.sort();
    exports.dedup();
    exports
}

/// Shared identity values for hooks, tracked executions and external processes.
fn workspace_identity(workspace: &Workspace, paths: &Paths) -> BTreeMap<String, OsString> {
    BTreeMap::from([
        (WORKSPACE_ID.into(), workspace.id.clone().into()),
        (RUN_ID.into(), workspace.id.clone().into()),
        (WORKSPACE_NAME.into(), workspace.name.clone().into()),
        (STATE_DIR.into(), paths.state.as_os_str().to_owned()),
    ])
}

/// The workspace environment, without a tracked execution marker.
pub fn workspace_environment(
    workspace: &Workspace,
    paths: &Paths,
    ports: &[PortReservation],
    token: &str,
) -> BTreeMap<String, OsString> {
    let mut values = workspace_identity(workspace, paths);
    values.insert(SCOPE_TOKEN.into(), token.into());
    values.insert(
        RESERVED_PORT_ENV.into(),
        ports
            .iter()
            .map(|port| port.env_var.as_str())
            .collect::<Vec<_>>()
            .join(":")
            .into(),
    );
    values.extend(
        ports
            .iter()
            .map(|port| (port.env_var.clone(), port.port.to_string().into())),
    );
    values
}

/// Set shared workspace identity and clear inherited port exports and shell state.
pub fn apply_workspace_identity(command: &mut Command, workspace: &Workspace, paths: &Paths) {
    for name in inherited_port_exports() {
        command.env_remove(name);
    }
    command
        .envs(workspace_identity(workspace, paths))
        .env_remove(SHELL_DIRECTIVE);
}

pub fn scope_token() -> Option<String> {
    std::env::var(SCOPE_TOKEN).ok()
}

/// True when this process carries cooperative workspace scope.
pub fn is_scoped() -> bool {
    std::env::var_os(SCOPE_TOKEN).is_some()
}
