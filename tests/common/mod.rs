//! Integration tests must not see the environment they are launched from: a
//! Shoal execution's scope, or the developer's own configuration.
use std::{ffi::OsStr, process::Command};

/// Non-`SHOAL_` variables Shoal reads; tests that need one set it explicitly.
const INHERITED: &[&str] = &["XDG_CONFIG_HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME"];

/// A command without inherited Shoal variables or configuration locations.
pub fn isolated(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(|name| name.starts_with("SHOAL_") || INHERITED.contains(&name))
        {
            command.env_remove(name);
        }
    }
    command
}
