//! Environment variables Shoal reads or exports. Every `SHOAL_*` name lives
//! here so wrapper, daemon, and process scanning agree on the contract.
use anyhow::{Result, ensure};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};
use tokio::process::Command;

use crate::{
    config::placeholders,
    model::{PortReservation, Workspace},
    paths::Paths,
};

pub const PREFIX: &str = "SHOAL_";

/// Overrides the state directory; also isolates the daemon socket.
pub const STATE_DIR: &str = "SHOAL_STATE_DIR";
/// Private socket and lock descriptors transferred across a managed update.
pub(crate) const DAEMON_HANDOFF: &str = "SHOAL_DAEMON_HANDOFF";
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
/// JSON array of the ready-for-review marks passed to post-ready hooks.
pub const REVIEW_MARKS: &str = "SHOAL_REVIEW_MARKS";
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

/// Validate names configured in `[env]` before they reach a process.
pub fn validate_configured_environment(values: &BTreeMap<String, String>) -> Result<()> {
    for (name, value) in values {
        ensure!(
            name.bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "invalid [env] variable name: {name}"
        );
        ensure!(
            !name.starts_with(PREFIX),
            "[env] variable conflicts with Shoal's execution environment: {name}"
        );
        ensure!(!value.contains('\0'), "[env] value for {name} contains NUL");
    }
    Ok(())
}

/// Render configured values once for one workspace. Inserted values are never
/// scanned again, matching the prompt template substitution rules.
pub fn render_configured_environment(
    values: &BTreeMap<String, String>,
    workspace: &Workspace,
    repository_name: &str,
    ports: &[PortReservation],
) -> BTreeMap<String, OsString> {
    let mut fields = vec![
        ("{workspace}", std::ffi::OsStr::new(&workspace.name)),
        ("{workspace_path}", workspace.path.as_os_str()),
        ("{path}", workspace.path.as_os_str()),
        ("{repo}", std::ffi::OsStr::new(repository_name)),
        ("{branch}", std::ffi::OsStr::new(&workspace.branch)),
    ];
    let mut port_values = Vec::new();
    for port in ports {
        let value = port.port.to_string();
        port_values.push((format!("{{port.{}}}", port.name), value.clone()));
        port_values.push((format!("{{port_{}}}", port.name), value.clone()));
        port_values.push((format!("{{{}}}", port.name), value));
    }
    fields.extend(
        port_values
            .iter()
            .map(|(name, value)| (name.as_str(), std::ffi::OsStr::new(value))),
    );
    values
        .iter()
        .map(|(name, value)| (name.clone(), placeholders::render_os(value, &fields)))
        .collect()
}

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

/// True when this process or an ancestor carries scope for the `state`
/// directory, so clearing Shoal variables in a child does not lift its
/// execution's limits.
pub fn inherits_scope(state: &std::path::Path) -> bool {
    is_scoped()
        || crate::process::identity::scoped_ancestor_state_dirs()
            .iter()
            .any(|dir| same_directory(dir, state))
}

fn same_directory(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn configured_values_render_identity_and_only_reserved_ports_once() {
        let workspace = Workspace::new_record(
            "repo".into(),
            "task-{branch}".into(),
            PathBuf::from("/tmp/{repo}/$(false) 日本語"),
            "feature/session".into(),
            crate::state::WorkspaceState::Ready,
        );
        let ports = vec![PortReservation {
            workspace_id: workspace.id.clone(),
            name: "web".into(),
            port: 3000,
            env_var: "WEB_PORT".into(),
            reason: None,
        }];
        let values = BTreeMap::from([
            ("SESSION".into(), "{repo}:{workspace}:{branch}".into()),
            ("PROFILE".into(), "{workspace_path}/profile:{path}".into()),
            (
                "URL".into(),
                "http://localhost:{port.web}/{port.missing}".into(),
            ),
            (
                "LITERAL".into(),
                "$HOME $(false) {unknown} {SESSION}".into(),
            ),
            ("EMPTY".into(), "".into()),
        ]);
        let rendered =
            render_configured_environment(&values, &workspace, "project-{workspace}", &ports);
        assert_eq!(
            rendered["SESSION"],
            "project-{workspace}:task-{branch}:feature/session"
        );
        assert_eq!(
            rendered["PROFILE"],
            "/tmp/{repo}/$(false) 日本語/profile:/tmp/{repo}/$(false) 日本語"
        );
        assert_eq!(rendered["URL"], "http://localhost:3000/{port.missing}");
        assert_eq!(rendered["LITERAL"], "$HOME $(false) {unknown} {SESSION}");
        assert_eq!(rendered["EMPTY"], "");
    }

    #[test]
    fn configured_paths_preserve_non_utf8_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let workspace = Workspace::new_record(
            "repo".into(),
            "worker".into(),
            PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec())),
            "worker".into(),
            crate::state::WorkspaceState::Ready,
        );
        let values = BTreeMap::from([("PROFILE".into(), "{workspace_path}/profile".into())]);
        let rendered = render_configured_environment(&values, &workspace, "repo", &[]);
        assert_eq!(
            rendered["PROFILE"].as_os_str(),
            OsStr::from_bytes(b"/tmp/\xff/profile")
        );
    }
}
