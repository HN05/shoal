//! Environment variables Shoal reads or exports. Every `SHOAL_*` name lives
//! here so wrapper, daemon, process scanning and apps agree on the contract.
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use anyhow::{Result, ensure};

pub const PREFIX: &str = "SHOAL_";

/// Overrides the state directory; also isolates the daemon socket.
pub const STATE_DIR: &str = "SHOAL_STATE_DIR";
/// Resolved launch plan a new Herdr tab or zmx session hands its worker;
/// removed at startup.
pub const ADD_PLAN: &str = "SHOAL_ADD_PLAN";
/// The agent queue entry whose slot a launched agent takes over when it
/// registers; passed to launch workers and removed at startup.
pub const QUEUE_ENTRY: &str = "SHOAL_QUEUE_ENTRY";
/// Set on a chat that a focused place beside the caller runs, which records
/// it in `attach`'s history once it registers; removed at startup.
pub const SHOWN_CHAT: &str = "SHOAL_SHOWN_CHAT";
/// Set on the command a detached zmx session's shell runs, which ends the
/// session once its agent or workspace shell has run; removed at startup.
pub const ENDS_SESSION: &str = "SHOAL_ENDS_SESSION";
/// Set on the image an execution wrapper replaces itself with to become
/// responsible for macOS privacy access; removed at startup.
#[cfg(target_os = "macos")]
pub const RESPONSIBILITY_CLAIMED: &str = "SHOAL_RESPONSIBILITY_CLAIMED";
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
/// Set on an agent hook that waits in the background for the agent's next
/// update, outside the agent's process group, without doing work.
pub const WAITING_HOOK: &str = "SHOAL_WAITING_HOOK";
/// Recorded worktree path, including after removal.
pub const WORKSPACE_PATH: &str = "SHOAL_WORKSPACE_PATH";
/// JSON resource lease passed to permit hooks.
pub const RESOURCE_LEASE: &str = "SHOAL_RESOURCE_LEASE";
/// Requested completion policy passed to post-done hooks: `keep` or `cleanup`.
pub const DONE_CHOICE: &str = "SHOAL_DONE_CHOICE";
/// JSON array of the ready-for-review marks passed to post-ready hooks.
pub const REVIEW_MARKS: &str = "SHOAL_REVIEW_MARKS";
/// Marks a one-shot agent run, such as a review, and the processes it starts
/// with the run's directory, so the daemon stops only them after it was
/// killed.
pub const ONESHOT_RUN: &str = "SHOAL_ONESHOT_RUN";
/// Shortcut or configured agent name passed to agent-exit hooks.
pub const AGENT: &str = "SHOAL_AGENT";
/// Reported exit code, empty when the agent disconnected without reporting.
pub const AGENT_EXIT_CODE: &str = "SHOAL_AGENT_EXIT_CODE";
/// Whether the tracked execution finished without surviving processes.
pub const AGENT_EXIT_COMPLETE: &str = "SHOAL_AGENT_EXIT_COMPLETE";
/// File the shell wrapper reads to change directory after the command exits.
pub const SHELL_DIRECTIVE: &str = "SHOAL_SHELL_DIRECTIVE";
/// The terminal place, as JSON, that a start the daemon runs later for an
/// agent queue entry comes from.
pub const ORIGIN: &str = "SHOAL_ORIGIN";
/// The output format, `json` or `text`, of a command that selects neither
/// `--json` nor `--text`; Shoal sets `json` for the agents it launches.
pub const OUTPUT: &str = "SHOAL_OUTPUT";
/// clap dynamic-completion trigger variable.
pub const COMPLETE: &str = "SHOAL_COMPLETE";
/// Set to `1` to print timing lines for profiling Shoal itself.
pub const PROFILE: &str = "SHOAL_PROFILE";
/// Set to `1` to let a debug build use the user's state directory and change
/// user-level files, for tests that run in a temporary home.
pub const DEV_USER_FILES: &str = "SHOAL_DEV_USER_FILES";
/// Runtime override for the packaged skills directory.
pub const SKILLS_DIR: &str = "SHOAL_SKILLS_DIR";
/// Claude Code's configuration directory override (its `.claude.json` and skills).
pub const CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
/// Codex's user configuration and state directory override.
pub const CODEX_HOME: &str = "CODEX_HOME";
/// Variables that say where agents keep their configuration, which Shoal
/// keeps when it trusts a workspace for a tracked agent, for the repository's
/// removal to clean the trust entries.
pub const AGENT_CONFIG_VARIABLES: [&str; 2] = [CLAUDE_CONFIG_DIR, CODEX_HOME];

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

/// The configured Claude Code directory, if any, of a process whose
/// environment adds `configured` to this one's; a relative override is an error.
pub fn claude_config_dir_with(configured: &BTreeMap<String, OsString>) -> Result<Option<PathBuf>> {
    claude_config_dir_from(|name| configured_var(configured, name))
}

/// The configured Claude Code directory, if any, of a process whose
/// variables `variable` reads; a relative override is an error.
pub fn claude_config_dir_from(
    variable: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<PathBuf>> {
    absolute_dir(CLAUDE_CONFIG_DIR, variable(CLAUDE_CONFIG_DIR))
}

/// The configured Codex directory, if any, of a process whose environment
/// adds `configured` to this one's; a relative override is an error.
pub fn codex_home_with(configured: &BTreeMap<String, OsString>) -> Result<Option<PathBuf>> {
    codex_home_from(|name| configured_var(configured, name))
}

/// The configured Codex directory, if any, of a process whose variables
/// `variable` reads; a relative override is an error.
pub fn codex_home_from(variable: impl Fn(&str) -> Option<OsString>) -> Result<Option<PathBuf>> {
    absolute_dir(CODEX_HOME, variable(CODEX_HOME))
}

/// Variable `name` as `configured` sets it for a launch, else as this
/// process has it.
pub(crate) fn configured_var(
    configured: &BTreeMap<String, OsString>,
    name: &str,
) -> Option<OsString> {
    configured
        .get(name)
        .cloned()
        .or_else(|| std::env::var_os(name))
}

fn absolute_dir(name: &str, value: Option<OsString>) -> Result<Option<PathBuf>> {
    let Some(dir) = value else {
        return Ok(None);
    };
    let dir = PathBuf::from(dir);
    ensure!(dir.is_absolute(), "{name} must be an absolute path");
    Ok(Some(dir))
}

/// The directory for temporary files, which Shoal sets to the workspace's
/// or chat's own.
pub const TMPDIR: &str = "TMPDIR";

/// Variables a reserved port may never shadow when exported to a command.
pub const PROTECTED: [&str; 4] = ["HOME", "PATH", "SHELL", TMPDIR];

/// The scope token a starter handed this process with a chat to start,
/// which stands in for [`SCOPE_TOKEN`] without entering any environment.
static HANDED_SCOPE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Take `token`, which a starter handed this process, as its scope token;
/// false when it already took one.
pub fn take_handed_scope(token: String) -> bool {
    HANDED_SCOPE.set(token).is_ok()
}

pub fn scope_token() -> Option<String> {
    (HANDED_SCOPE.get().cloned()).or_else(|| std::env::var(SCOPE_TOKEN).ok())
}

/// True when this process carries cooperative workspace scope.
pub fn is_scoped() -> bool {
    HANDED_SCOPE.get().is_some() || std::env::var_os(SCOPE_TOKEN).is_some()
}
