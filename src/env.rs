//! Environment variables Shoal reads or exports. Every `SHOAL_*` name lives
//! here so wrapper, daemon, and process scanning agree on the contract.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::PathBuf,
};
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
/// Resolved launch plan a new Herdr tab or zmx session hands its worker;
/// removed at startup.
pub const ADD_PLAN: &str = "SHOAL_ADD_PLAN";
/// The agent queue entry whose slot a launched agent takes over when it
/// registers; passed to launch workers and removed at startup.
pub const QUEUE_ENTRY: &str = "SHOAL_QUEUE_ENTRY";
/// Set on the command a detached zmx session's shell runs, which ends the
/// session once its agent or workspace shell has run; removed at startup.
pub const ENDS_SESSION: &str = "SHOAL_ENDS_SESSION";
/// The chat whose agent this process runs under; workspaces opened here belong
/// to that chat.
pub const CHAT: &str = "SHOAL_CHAT";
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
/// Colon-separated names of the secret variables an agent launch exported,
/// which hooks started beneath it drop.
pub const SECRET_ENV: &str = "SHOAL_SECRET_ENV";
/// Secret tool launcher directories an agent launch put on PATH, which hooks
/// started beneath it remove.
pub const SECRET_TOOLS: &str = "SHOAL_SECRET_TOOLS";
/// A rendered `[env]` entry that names directories to put first on the PATH
/// of the process it reaches instead of a variable to set; only presets
/// set it.
pub const PATH_PREFIX: &str = "SHOAL_PATH_PREFIX";
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
/// The output format, `json` or `text`, of a command that selects neither
/// `--json` nor `--text`; Shoal sets `json` for the agents it launches.
pub const OUTPUT: &str = "SHOAL_OUTPUT";
/// clap dynamic-completion trigger variable.
pub const COMPLETE: &str = "SHOAL_COMPLETE";
/// Set to `1` to print timing lines for profiling Shoal itself.
pub const PROFILE: &str = "SHOAL_PROFILE";
/// Set to `1` to let a debug build change user-level files, for tests that run
/// in a temporary home.
pub const DEV_USER_FILES: &str = "SHOAL_DEV_USER_FILES";
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
/// Variables that say where agents keep their configuration, which Shoal
/// keeps when it trusts a workspace for a tracked agent, for the repository's
/// removal to clean the trust entries.
pub const AGENT_CONFIG_VARIABLES: [&str; 2] = [CLAUDE_CONFIG_DIR, CODEX_HOME];

/// The [`AGENT_CONFIG_VARIABLES`] that `configured` sets; a value that is not
/// UTF-8 cannot be kept.
pub fn agent_config_environment(
    configured: &BTreeMap<String, OsString>,
) -> BTreeMap<String, String> {
    AGENT_CONFIG_VARIABLES
        .iter()
        .filter_map(|&name| {
            let value = configured.get(name)?.to_str()?;
            Some((name.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Validate names configured in `[env]` before they reach a process.
pub fn validate_configured_environment(values: &BTreeMap<String, String>) -> Result<()> {
    for (name, value) in values {
        validate_configured_name("[env]", name)?;
        ensure!(!value.contains('\0'), "[env] value for {name} contains NUL");
    }
    Ok(())
}

/// A variable name `table` may set: portable, and outside Shoal's own names.
pub fn validate_configured_name(table: &str, name: &str) -> Result<()> {
    ensure!(
        name.bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "invalid {table} variable name: {name}"
    );
    ensure!(
        !name.starts_with(PREFIX),
        "{table} variable conflicts with Shoal's execution environment: {name}"
    );
    Ok(())
}

/// Render configured values once for one workspace. Inserted values are never
/// scanned again, matching the prompt template substitution rules.
pub fn render_configured_environment(
    values: &BTreeMap<String, String>,
    workspace: &Workspace,
    repository_name: &str,
    ports: &[PortReservation],
    caches: &BTreeMap<String, PathBuf>,
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
        port_values.push((format!("{{port.{}}}", port.name), port.port.to_string()));
    }
    fields.extend(
        port_values
            .iter()
            .map(|(name, value)| (name.as_str(), std::ffi::OsStr::new(value))),
    );
    let cache_keys: Vec<_> = caches
        .iter()
        .map(|(name, directory)| (format!("{{cache.{name}}}"), directory))
        .collect();
    fields.extend(
        cache_keys
            .iter()
            .map(|(key, directory)| (key.as_str(), directory.as_os_str())),
    );
    values
        .iter()
        .map(|(name, value)| (name.clone(), placeholders::render_os(value, &fields)))
        .collect()
}

/// The configured Claude Code directory, if any; a relative override is an error.
pub fn claude_config_dir() -> Result<Option<PathBuf>> {
    absolute_dir_var(CLAUDE_CONFIG_DIR)
}

/// [`claude_config_dir`] of a process whose environment adds `configured`
/// to this one's.
pub fn claude_config_dir_with(configured: &BTreeMap<String, OsString>) -> Result<Option<PathBuf>> {
    absolute_dir(
        CLAUDE_CONFIG_DIR,
        configured_var(configured, CLAUDE_CONFIG_DIR),
    )
}

/// The configured Codex directory, if any, of a process whose environment
/// adds `configured` to this one's; a relative override is an error.
pub fn codex_home_with(configured: &BTreeMap<String, OsString>) -> Result<Option<PathBuf>> {
    absolute_dir(CODEX_HOME, configured_var(configured, CODEX_HOME))
}

fn configured_var(configured: &BTreeMap<String, OsString>, name: &str) -> Option<OsString> {
    configured
        .get(name)
        .cloned()
        .or_else(|| std::env::var_os(name))
}

fn absolute_dir_var(name: &str) -> Result<Option<PathBuf>> {
    absolute_dir(name, std::env::var_os(name))
}

fn absolute_dir(name: &str, value: Option<OsString>) -> Result<Option<PathBuf>> {
    let Some(dir) = value else {
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

/// Secret variables inherited from an enclosing agent launch.
pub fn inherited_secret_exports() -> Vec<String> {
    std::env::var(SECRET_ENV)
        .unwrap_or_default()
        .split(':')
        .filter(|name| !name.is_empty() && !PROTECTED.contains(name) && !name.starts_with(PREFIX))
        .map(String::from)
        .collect()
}

/// The inherited PATH without secret tool launchers, when an enclosing agent
/// launch added any.
pub fn path_without_secret_tools() -> Option<OsString> {
    let directories: Vec<_> = std::env::split_paths(&std::env::var_os(SECRET_TOOLS)?)
        .filter(|directory| !directory.as_os_str().is_empty())
        .collect();
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::env::split_paths(&path).filter(|entry| {
        !directories
            .iter()
            .any(|directory| same_directory(directory, entry))
    }))
    .ok()
}

/// Set rendered `[env]` values on `command`, putting a preset's directories
/// first on the PATH it would otherwise get.
pub fn apply_configured(command: &mut Command, values: &BTreeMap<String, OsString>) {
    for (name, value) in values {
        if name == PATH_PREFIX {
            prepend_path(command, value);
        } else {
            command.env(name, value);
        }
    }
}

/// Rendered `[env]` values as a shell or another program sets them, with a
/// preset's directories first on the configured PATH, or else `inherited`.
pub fn export_configured(
    mut values: BTreeMap<String, String>,
    inherited: Option<OsString>,
) -> Result<BTreeMap<String, String>> {
    if let Some(prefix) = values.remove(PATH_PREFIX) {
        let path = values
            .get("PATH")
            .map(OsString::from)
            .or(inherited)
            .unwrap_or_default();
        let joined = joined_path(OsStr::new(&prefix), &path)
            .ok()
            .and_then(|joined| joined.into_string().ok());
        let joined = joined.context("PATH cannot be exported with the preset directories")?;
        values.insert("PATH".into(), joined);
    }
    Ok(values)
}

/// Put the colon-separated `directories` first on the PATH `command` would
/// get, ahead of any directories an earlier step put there. Directories that
/// cannot join a PATH leave it unchanged.
pub fn prepend_path(command: &mut Command, directories: &OsStr) {
    let current = command
        .as_std()
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value.map(OsStr::to_owned))
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    if let Ok(path) = joined_path(directories, &current) {
        command.env("PATH", path);
    }
}

pub fn joined_path(
    directories: &OsStr,
    path: &OsStr,
) -> Result<OsString, std::env::JoinPathsError> {
    std::env::join_paths(
        std::env::split_paths(directories)
            .chain(std::env::split_paths(path))
            .filter(|entry| !entry.as_os_str().is_empty()),
    )
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

/// A chat agent's identity: its chat, and the state directory whose daemon
/// recorded it, which the agent's own Shoal commands must reach; and the
/// agents' output default.
pub fn chat_environment(chat: &crate::model::Chat, paths: &Paths) -> BTreeMap<String, OsString> {
    let mut values = BTreeMap::from([
        (CHAT.into(), chat.id.clone().into()),
        (STATE_DIR.into(), paths.state.as_os_str().to_owned()),
    ]);
    values.extend(agent_output().map(|(name, value)| (name.into(), value.into())));
    values
}

/// The workspace environment, without a tracked execution marker.
pub fn workspace_environment(
    workspace: &Workspace,
    paths: &Paths,
    ports: &[PortReservation],
    token: &str,
) -> BTreeMap<String, OsString> {
    let mut values = workspace_identity(workspace, paths);
    values.insert(WORKSPACE_PATH.into(), workspace.path.as_os_str().to_owned());
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

/// Whether an [`OUTPUT`] setting selects JSON; unset or empty selects text.
pub fn selects_json(setting: Option<&OsStr>) -> Result<bool> {
    match setting.map(OsStr::to_str) {
        None | Some(Some("" | "text")) => Ok(false),
        Some(Some("json")) => Ok(true),
        Some(_) => bail!(
            "{OUTPUT} must be `json` or `text`, not {:?}",
            setting.unwrap_or_default()
        ),
    }
}

/// The [`OUTPUT`] default an agent Shoal launches gets: JSON, unless this
/// process already carries a setting, which the agent inherits instead.
pub fn agent_output() -> Option<(&'static str, &'static str)> {
    std::env::var_os(OUTPUT)
        .is_none()
        .then_some((OUTPUT, "json"))
}

pub fn scope_token() -> Option<String> {
    std::env::var(SCOPE_TOKEN).ok()
}

/// True when this process carries cooperative workspace scope.
pub fn is_scoped() -> bool {
    std::env::var_os(SCOPE_TOKEN).is_some()
}

/// The chat this process runs under, if any.
pub fn chat() -> Option<String> {
    std::env::var(CHAT).ok().filter(|chat| !chat.is_empty())
}

/// True when this process or an ancestor carries scope for the `state`
/// directory, so clearing Shoal variables in a child does not lift its
/// execution's limits. Tracked executions record their processes; an exported
/// environment is found only where ancestors' environments are readable.
pub fn inherits_scope(state: &std::path::Path) -> bool {
    is_scoped()
        || crate::process::scoped::recorded_ancestor(state)
        || crate::process::identity::scoped_ancestor_state_dirs()
            .iter()
            .any(|dir| same_directory(dir, state))
}

pub fn same_directory(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

static LAUNCH_QUEUE_ENTRY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Keep the queue entry a launching process passed down, and drop it from the
/// environment so the agent's own children do not inherit it.
///
/// # Safety
/// Call before any other thread exists.
pub(crate) unsafe fn take_queue_entry() {
    if let Some(entry) = std::env::var(QUEUE_ENTRY)
        .ok()
        .filter(|entry| !entry.is_empty())
    {
        let _ = LAUNCH_QUEUE_ENTRY.set(entry);
    }
    // SAFETY: the caller guarantees no other thread reads the environment.
    unsafe { std::env::remove_var(QUEUE_ENTRY) };
}

static ENDS_LAUNCH_SESSION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Keep whether this process ends the session it runs in, and drop the marker
/// from the environment so the agent's own children do not inherit it.
///
/// # Safety
/// Call before any other thread exists.
pub(crate) unsafe fn take_session_end() {
    if std::env::var_os(ENDS_SESSION).is_some_and(|value| !value.is_empty()) {
        ENDS_LAUNCH_SESSION.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // SAFETY: the caller guarantees no other thread reads the environment.
    unsafe { std::env::remove_var(ENDS_SESSION) };
}

/// The exit status to report for `code`, the exit of the agent or shell this
/// process ran: success when that ends its session, which reads only success.
pub(crate) fn session_exit_code(code: i32) -> i32 {
    if ENDS_LAUNCH_SESSION.load(std::sync::atomic::Ordering::Relaxed) {
        0
    } else {
        code
    }
}

/// Record the queue entry whose slot the agent this process launches takes.
pub(crate) fn launch_queue_entry(entry: &str) {
    let _ = LAUNCH_QUEUE_ENTRY.set(entry.to_owned());
}

/// The queue entry an agent launched from this process takes over.
pub(crate) fn queue_entry() -> Option<&'static str> {
    LAUNCH_QUEUE_ENTRY.get().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_of(command: &Command) -> Option<OsString> {
        command
            .as_std()
            .get_envs()
            .find(|(name, _)| *name == "PATH")
            .and_then(|(_, value)| value.map(OsStr::to_owned))
    }

    #[test]
    fn preset_directories_go_first_on_the_path_a_process_gets() {
        let mut command = Command::new("true");
        command.env("PATH", "/usr/bin:/bin");
        let values = BTreeMap::from([
            (PATH_PREFIX.to_owned(), OsString::from("/cache/xcode/bin")),
            ("GOFLAGS".to_owned(), OsString::from("-trimpath")),
        ]);
        apply_configured(&mut command, &values);
        assert_eq!(path_of(&command).unwrap(), "/cache/xcode/bin:/usr/bin:/bin");
        assert!(
            command
                .as_std()
                .get_envs()
                .all(|(name, _)| name != PATH_PREFIX)
        );
        // A later step, such as the agent account's wrappers, goes before it.
        prepend_path(&mut command, OsStr::new("/wrappers"));
        assert_eq!(
            path_of(&command).unwrap(),
            "/wrappers:/cache/xcode/bin:/usr/bin:/bin"
        );

        let exported = export_configured(
            BTreeMap::from([(PATH_PREFIX.to_owned(), "/cache/xcode/bin".to_owned())]),
            Some("/usr/bin".into()),
        )
        .unwrap();
        assert_eq!(
            exported,
            BTreeMap::from([("PATH".to_owned(), "/cache/xcode/bin:/usr/bin".to_owned())])
        );
        // A PATH `[env]` sets is the one the directories go before.
        let exported = export_configured(
            BTreeMap::from([
                (PATH_PREFIX.to_owned(), "/cache/xcode/bin".to_owned()),
                ("PATH".to_owned(), "/tools".to_owned()),
            ]),
            Some("/usr/bin".into()),
        )
        .unwrap();
        assert_eq!(exported["PATH"], "/cache/xcode/bin:/tools");
    }

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
            ("GOCACHE".into(), "{cache.go}:{cache.missing}".into()),
            (
                "LITERAL".into(),
                "$HOME $(false) {unknown} {SESSION}".into(),
            ),
            ("EMPTY".into(), "".into()),
        ]);
        let caches = BTreeMap::from([("go".into(), PathBuf::from("/tmp/.caches/{cache.go}"))]);
        let rendered = render_configured_environment(
            &values,
            &workspace,
            "project-{workspace}",
            &ports,
            &caches,
        );
        assert_eq!(
            rendered["SESSION"],
            "project-{workspace}:task-{branch}:feature/session"
        );
        assert_eq!(
            rendered["PROFILE"],
            "/tmp/{repo}/$(false) 日本語/profile:/tmp/{repo}/$(false) 日本語"
        );
        assert_eq!(rendered["URL"], "http://localhost:3000/{port.missing}");
        assert_eq!(
            rendered["GOCACHE"],
            "/tmp/.caches/{cache.go}:{cache.missing}"
        );
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
        let rendered =
            render_configured_environment(&values, &workspace, "repo", &[], &BTreeMap::new());
        assert_eq!(
            rendered["PROFILE"].as_os_str(),
            OsStr::from_bytes(b"/tmp/\xff/profile")
        );
    }
}
