//! The environment Shoal gives the processes it starts, and what it reads of
//! its own; [`crate::client::env`] names every variable.
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};
use tokio::process::Command;

pub(crate) use crate::client::env::*;
use crate::{
    config::placeholders,
    model::{PortReservation, Workspace},
    paths::Paths,
};

/// Private socket and lock descriptors transferred across a managed update.
pub(crate) const DAEMON_HANDOFF: &str = "SHOAL_DAEMON_HANDOFF";

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

/// Render configured values once for one workspace, after `tmp_dir`, its
/// temporary directory, as [`TMPDIR`], which `[env]` may override. Inserted
/// values are never scanned again, matching the prompt template substitution
/// rules.
pub fn render_configured_environment(
    values: &BTreeMap<String, String>,
    workspace: &Workspace,
    repository_name: &str,
    ports: &[PortReservation],
    caches: &BTreeMap<String, PathBuf>,
    tmp_dir: Option<&Path>,
) -> BTreeMap<String, OsString> {
    let mut fields = vec![
        ("{workspace}", std::ffi::OsStr::new(&workspace.name)),
        ("{workspace_path}", workspace.path.as_os_str()),
        ("{path}", workspace.path.as_os_str()),
        ("{repo}", std::ffi::OsStr::new(repository_name)),
        (
            "{branch}",
            std::ffi::OsStr::new(workspace.branch.as_deref().unwrap_or_default()),
        ),
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
    render_environment(values, fields, caches, tmp_dir)
}

/// [`render_configured_environment`] for a chat, which runs in the
/// registered checkout at `checkout` and has no workspace, branch or ports.
pub fn render_chat_environment(
    values: &BTreeMap<String, String>,
    checkout: &Path,
    repository_name: &str,
    caches: &BTreeMap<String, PathBuf>,
    tmp_dir: Option<&Path>,
) -> BTreeMap<String, OsString> {
    let fields = vec![
        ("{path}", checkout.as_os_str()),
        ("{repo}", std::ffi::OsStr::new(repository_name)),
    ];
    render_environment(values, fields, caches, tmp_dir)
}

/// `values` with `fields` and each cache's `{cache.<name>}` filled in, after
/// `TMPDIR` when there is a temporary directory.
fn render_environment(
    values: &BTreeMap<String, String>,
    fields: Vec<(&str, &std::ffi::OsStr)>,
    caches: &BTreeMap<String, PathBuf>,
    tmp_dir: Option<&Path>,
) -> BTreeMap<String, OsString> {
    let cache_keys: Vec<_> = caches
        .iter()
        .map(|(name, directory)| (format!("{{cache.{name}}}"), directory))
        .collect();
    let mut fields = fields;
    fields.extend(
        cache_keys
            .iter()
            .map(|(key, directory)| (key.as_str(), directory.as_os_str())),
    );
    let mut rendered: BTreeMap<_, _> = tmp_dir
        .map(|directory| (TMPDIR.to_owned(), directory.as_os_str().to_owned()))
        .into_iter()
        .collect();
    rendered.extend(
        values
            .iter()
            .map(|(name, value)| (name.clone(), placeholders::render_os(value, &fields))),
    );
    rendered
}

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

/// This process's environment for work another process does on its behalf:
/// without the secrets an enclosing agent launch exported, or their
/// launchers on the PATH, so no secret travels through the daemon. Values
/// that are not UTF-8 stay behind.
pub fn without_secrets() -> BTreeMap<String, String> {
    let secrets = inherited_secret_exports();
    let path = path_without_secret_tools();
    std::env::vars_os()
        .filter_map(|(name, value)| {
            let value = match (name.to_str()?, &path) {
                ("PATH", Some(path)) => path.clone(),
                _ => value,
            };
            Some((name.into_string().ok()?, value.into_string().ok()?))
        })
        .filter(|(name, _)| !secrets.contains(name) && name != SECRET_ENV && name != SECRET_TOOLS)
        .collect()
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

/// Give `command` the identity of `chat`: the chat and the state directory
/// whose daemon recorded it, as its hooks receive them.
pub fn apply_chat_identity(command: &mut Command, chat: &crate::model::Chat, paths: &Paths) {
    for name in not_chat_identity() {
        command.env_remove(name);
    }
    command.env(CHAT, &chat.id).env(STATE_DIR, &paths.state);
}

/// What a chat's processes leave out of this process's environment: a
/// workspace's identity and port exports, which a shell Shoal opened in a
/// workspace carries, and the shell's directive.
pub fn not_chat_identity() -> Vec<OsString> {
    let mut names = inherited_port_exports();
    names.extend(
        [
            EXECUTION_ID,
            WORKSPACE_ID,
            WORKSPACE_PATH,
            RUN_ID,
            WORKSPACE_NAME,
            SHELL_DIRECTIVE,
        ]
        .map(OsString::from),
    );
    names
}

/// A chat agent's identity: its chat, the scope `token` that confines it
/// to the chat and the spaces below it, and the state directory whose daemon
/// recorded it, which the agent's own Shoal commands must reach; its
/// temporary directory; and the agents' output default.
pub fn chat_environment(
    chat: &crate::model::Chat,
    token: &str,
    paths: &Paths,
) -> BTreeMap<String, OsString> {
    let mut values = BTreeMap::from([
        (CHAT.into(), chat.id.clone().into()),
        (SCOPE_TOKEN.into(), token.into()),
        (STATE_DIR.into(), paths.state.as_os_str().to_owned()),
    ]);
    if let Some(directory) = &chat.tmp_dir {
        values.insert(TMPDIR.into(), directory.as_os_str().to_owned());
    }
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
/// process already carries a nonempty setting, which the agent inherits instead.
pub fn agent_output() -> Option<(&'static str, &'static str)> {
    agent_output_under(std::env::var_os(OUTPUT).as_deref())
}

fn agent_output_under(setting: Option<&OsStr>) -> Option<(&'static str, &'static str)> {
    setting
        .is_none_or(OsStr::is_empty)
        .then_some((OUTPUT, "json"))
}

/// This process's [`OUTPUT`] setting, empty when it has none, for a launch
/// that starts in another environment, such as the daemon's, to use in place
/// of that environment's own.
pub fn output_setting() -> (String, String) {
    (OUTPUT.into(), std::env::var(OUTPUT).unwrap_or_default())
}

/// The chat this process runs under, if any.
pub fn chat() -> Option<String> {
    std::env::var(CHAT).ok().filter(|chat| !chat.is_empty())
}

/// Whether this process carries a workspace's scope, which names its own
/// workspace, rather than a chat's or none.
pub fn workspace_scoped() -> bool {
    is_scoped() && in_chat().is_none()
}

/// The chat whose own agent this process belongs to: a workspace's process
/// that a chat started, which keeps the chat's marker, belongs to the
/// workspace instead.
pub fn in_chat() -> Option<String> {
    own_chat(std::env::var_os(WORKSPACE_ID), chat())
}

/// `chat`, unless `workspace` names the workspace the process belongs to.
fn own_chat(workspace: Option<OsString>, chat: Option<String>) -> Option<String> {
    chat.filter(|_| workspace.is_none_or(|workspace| workspace.is_empty()))
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

static SHOWN_LAUNCH_CHAT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keep whether this process runs a chat in a place shown to the caller, and
/// drop the marker from the environment so the agent's own children do not
/// inherit it.
///
/// # Safety
/// Call before any other thread exists.
pub(crate) unsafe fn take_shown_chat() {
    if std::env::var_os(SHOWN_CHAT).is_some_and(|value| !value.is_empty()) {
        SHOWN_LAUNCH_CHAT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // SAFETY: the caller guarantees no other thread reads the environment.
    unsafe { std::env::remove_var(SHOWN_CHAT) };
}

/// Whether the chat this process starts runs in a place shown to the caller,
/// as [`take_shown_chat`] read it.
pub(crate) fn shown_chat() -> bool {
    SHOWN_LAUNCH_CHAT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether this process ends the session it runs in, as
/// [`take_session_end`] read it.
pub(crate) fn ends_launch_session() -> bool {
    ENDS_LAUNCH_SESSION.load(std::sync::atomic::Ordering::Relaxed)
}

/// The exit status to report for `code`, the exit of the agent or shell this
/// process ran: success when that ends its session, which reads only success.
pub(crate) fn session_exit_code(code: i32) -> i32 {
    if ends_launch_session() { 0 } else { code }
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
mod tests;
