//! Shared fixtures and process defaults for integration tests. Apply overrides with
//! `Command::env` / `current_dir` / `stdin` after constructing a command.
// Each integration test binary uses its own subset of support.
#![allow(dead_code)]

pub mod daemon;
pub mod daemon_client;
pub mod fake_daemon;
pub mod fixture;
pub mod forge;
pub mod home;
pub mod ports;
pub mod prompt;
pub mod pty;
pub mod run;
pub mod wait;
pub mod zmx;

// Each test binary uses its own subset of support, this included.
#[allow(unused_imports)]
pub use shoal_core::test_support::{
    git, git_with_identity, install_test_script, install_test_script_with_mode, printed,
    repository_with, set_config,
};

use shoal_core::test_support::{copy_in_child, isolate, with_developer_tools};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

/// A generous bound on a daemon socket read or write, never a timing assertion.
pub const HANG_GUARD: Duration = Duration::from_secs(60);

/// Copy the `shoal` binary to `destination`, an executable a test runs in
/// place of the one Cargo built.
pub fn copy_shoal_binary(destination: &Path) {
    copy_in_child(Path::new(shoal_binary()), destination);
}

/// A fake agent's script: it touches `marker` in its directory, then runs
/// until it is terminated.
pub fn fake_agent_script(marker: &str) -> String {
    format!("trap 'exit 0' TERM\ntouch {marker}\nwhile :; do sleep 1; done\n")
}

/// Install `agent.sh` in `dir`, a fake agent that touches `agent-started`.
pub fn install_fake_agent(dir: &Path) {
    fs::write(dir.join("agent.sh"), fake_agent_script("agent-started")).unwrap();
}

/// Keep commands and their descendants in a temporary home, state and cwd,
/// as [`isolate`] does, with terminal host sessions of their own. Preserve
/// PATH for test tooling, preferring executables in the fixture's bin, then
/// the fake hosts of [`runner_path`].
pub fn isolated(root: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    isolate(&mut command, root);
    let path = std::env::join_paths(
        std::iter::once(root.join("bin")).chain(std::env::split_paths(&runner_path())),
    )
    .unwrap();
    command
        .env("SHOAL_STATE_DIR", root.join("state"))
        // A launch that reaches a real zmx must not use the user's sessions.
        .env("ZMX_DIR", root.join("zmx"))
        // Nor a tmux one the user's server; the directory name stays short,
        // since a socket path is.
        .env("TMUX_TMPDIR", root.join("t"))
        // Nor a Zellij one the user's sessions.
        .env("ZELLIJ_SOCKET_DIR", root.join("z"))
        .env("PATH", path);
    command
}

/// The runner's PATH [`with_developer_tools`], behind `fixtures/hosts`,
/// whose fake terminal hosts answer in place of the runner's own, so no test
/// starts a session of the machine's zmx whether or not it has one. The
/// package's directory is read at run time, as [`shoal_binary`] is.
pub fn runner_path() -> OsString {
    let hosts = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from)
        .join("tests/fixtures/hosts");
    let runner = with_developer_tools(&std::env::var_os("PATH").unwrap_or_default());
    std::env::join_paths(std::iter::once(hosts).chain(std::env::split_paths(&runner))).unwrap()
}

pub fn cli(root: &Path) -> Command {
    isolated(root, shoal_binary())
}

/// Give `command` a stdout pipe whose reader has closed before it starts. The
/// child makes the pipe after it forks: a reader closed here could still be
/// open in another test's child forked in the meantime, until that one execs,
/// and a write into the pipe would succeed.
pub fn close_stdout(command: &mut Command) -> &mut Command {
    use std::os::unix::process::CommandExt;
    // SAFETY: pipe, close and dup2 are async-signal-safe and touch no memory
    // but the array on the stack. Stdio is set up before this runs.
    unsafe {
        command.stdout(Stdio::null()).pre_exec(|| {
            let mut ends = [0; 2];
            if libc::pipe(ends.as_mut_ptr()) != 0
                || libc::close(ends[0]) != 0
                || libc::dup2(ends[1], libc::STDOUT_FILENO) < 0
                || libc::close(ends[1]) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    }
}

/// The `shoal` binary Cargo built for this test run. Cargo also sets the
/// variable at run time, and that value wins: a test binary that a workspace
/// keeps from its seed build is not rebuilt for a new binary path, so its
/// compile-time value names the seed worktree's binary.
pub fn shoal_binary() -> &'static str {
    static BINARY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| {
        std::env::var("CARGO_BIN_EXE_shoal")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_shoal").to_owned())
    })
}
