//! Shared fixtures and process defaults for integration tests. Apply overrides with
//! `Command::env` / `current_dir` / `stdin` after constructing a command.
pub mod daemon_client;
pub mod fake_daemon;

#[allow(dead_code)]
pub mod wait;

use std::{
    ffi::OsStr,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

/// A generous bound on a daemon socket read or write, never a timing assertion.
#[allow(dead_code)]
pub const HANG_GUARD: Duration = Duration::from_secs(60);

/// Install an executable fixture, including any parent directories it needs.
#[allow(dead_code)] // Integration test binaries use different subsets of support.
pub fn install_test_script(path: impl AsRef<Path>, script: impl AsRef<[u8]>) {
    install_test_script_with_mode(path, script, 0o755);
}

/// Install an executable fixture with an explicit Unix permission mode.
#[allow(dead_code)]
pub fn install_test_script_with_mode(path: impl AsRef<Path>, script: impl AsRef<[u8]>, mode: u32) {
    let path = std::path::absolute(path).unwrap();
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    // Copy in a child so parallel spawns cannot inherit the destination's
    // writable file descriptor and cause ETXTBSY when executing the fixture.
    let mut source = tempfile::NamedTempFile::new_in(parent).unwrap();
    source.write_all(script.as_ref()).unwrap();
    let output = isolated(parent, "cp")
        .arg(source.path())
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Keep commands and their descendants in a temporary home, state and cwd.
/// Preserve PATH for test tooling, preferring executables in the fixture's bin.
pub fn isolated(root: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            ["SHOAL_", "XDG_", "HAPPY_", "GIT_", "HERDR_", "ZMX_", "REX_"]
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
        // Whatever stdin the runner has must not reach a test: a command that
        // reads an open pipe or socket to its end would wait forever.
        .stdin(Stdio::null())
        .current_dir(root)
        .env("HOME", root)
        .env("ZDOTDIR", root)
        .env("SHOAL_STATE_DIR", root.join("state"))
        // A launch that reaches a real zmx must not use the user's sessions.
        .env("ZMX_DIR", root.join("zmx"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("PATH", path);
    command
}

pub fn cli(root: &Path) -> Command {
    isolated(root, env!("CARGO_BIN_EXE_shoal"))
}
