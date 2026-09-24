//! Process defaults for integration tests. Apply intentional overrides with
//! `Command::env` / `current_dir` after constructing a command.
use std::{ffi::OsStr, path::Path, process::Command};

/// Keep commands and their descendants in a temporary home, state and cwd.
/// Preserve PATH for test tooling, preferring executables in the fixture's bin.
pub fn isolated(root: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            ["SHOAL_", "XDG_", "HAPPY_", "GIT_"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
                || ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "BASH_ENV", "ENV"].contains(&name)
        }) {
            command.env_remove(name);
        }
    }
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    command
        .current_dir(root)
        .env("HOME", root)
        .env("ZDOTDIR", root)
        .env("SHOAL_STATE_DIR", root.join("state"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("PATH", path);
    command
}

pub fn cli(root: &Path) -> Command {
    isolated(root, env!("CARGO_BIN_EXE_shoal"))
}
