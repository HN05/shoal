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
    // Copy to a fresh file so an earlier fixture can still be executing.
    // Close its writable descriptor before copying in a child, so parallel
    // spawns cannot inherit it and cause ETXTBSY when executing the fixture.
    let destination = tempfile::NamedTempFile::new_in(parent)
        .unwrap()
        .into_temp_path();
    let mut source = tempfile::NamedTempFile::new_in(parent).unwrap();
    source.write_all(script.as_ref()).unwrap();
    let output = isolated(parent, "cp")
        .arg(source.path())
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::set_permissions(&destination, fs::Permissions::from_mode(mode)).unwrap();
    fs::rename(&destination, path).unwrap();
}

/// Keep commands and their descendants in a temporary home, state and cwd.
/// Preserve PATH for test tooling, preferring executables in the fixture's bin.
pub fn isolated(root: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            [
                "SHOAL_", "XDG_", "HAPPY_", "GIT_", "HERDR_", "ZMX_", "REX_", "TMUX", "ZELLIJ",
            ]
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
        // Nor a tmux one the user's server; the directory name stays short,
        // since a socket path is.
        .env("TMUX_TMPDIR", root.join("t"))
        // Nor a Zellij one the user's sessions.
        .env("ZELLIJ_SOCKET_DIR", root.join("z"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("PATH", path);
    command
}

pub fn cli(root: &Path) -> Command {
    isolated(root, shoal_binary())
}

/// Give `command` a stdout pipe whose reader has closed before it starts. The
/// child makes the pipe after it forks: a reader closed here could still be
/// open in another test's child forked in the meantime, until that one execs,
/// and a write into the pipe would succeed.
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn shoal_binary() -> &'static str {
    static BINARY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| {
        std::env::var("CARGO_BIN_EXE_shoal")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_shoal").to_owned())
    })
}
