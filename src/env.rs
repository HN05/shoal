//! Environment variables Shoal reads or exports. Every `SHOAL_*` name lives
//! here so wrapper, daemon, and process scanning agree on the contract.
use anyhow::{Result, ensure};
use std::path::PathBuf;

/// Overrides the state directory; also isolates the daemon socket.
pub const STATE_DIR: &str = "SHOAL_STATE_DIR";
/// Cooperative scope token given to processes launched through the wrapper.
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
/// Which lifecycle hook is running: `post_setup` or `pre_remove`.
pub const HOOK: &str = "SHOAL_HOOK";
/// File the shell wrapper reads to change directory after the command exits.
pub const SHELL_DIRECTIVE: &str = "SHOAL_SHELL_DIRECTIVE";
/// The shell wrapper's `OLDPWD`, passed explicitly because it is shell-local.
pub const PREVIOUS_DIR: &str = "SHOAL_PREVIOUS_DIR";
/// clap dynamic-completion trigger variable.
pub const COMPLETE: &str = "SHOAL_COMPLETE";
/// Claude Code's configuration directory override (its `.claude.json` and skills).
pub const CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
/// Codex's user configuration and state directory override.
pub const CODEX_HOME: &str = "CODEX_HOME";

pub fn codex_home() -> Result<Option<PathBuf>> {
    let Some(dir) = std::env::var_os(CODEX_HOME) else {
        return Ok(None);
    };
    let dir = PathBuf::from(dir);
    ensure!(dir.is_absolute(), "{CODEX_HOME} must be an absolute path");
    Ok(Some(dir))
}

/// The configured Claude Code directory, if any; a relative override is an error.
pub fn claude_config_dir() -> Result<Option<PathBuf>> {
    let Some(dir) = std::env::var_os(CLAUDE_CONFIG_DIR) else {
        return Ok(None);
    };
    let dir = PathBuf::from(dir);
    ensure!(
        dir.is_absolute(),
        "{CLAUDE_CONFIG_DIR} must be an absolute path"
    );
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
        && (!name.starts_with("SHOAL_") || name.starts_with(PORT_PREFIX))
}

pub fn scope_token() -> Option<String> {
    std::env::var(SCOPE_TOKEN).ok()
}

/// True when this process runs inside a tracked workspace execution.
pub fn is_scoped() -> bool {
    std::env::var_os(SCOPE_TOKEN).is_some()
}
