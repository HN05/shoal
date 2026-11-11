//! The base each test binary's `Fixture` wraps: an isolated home whose forge
//! requests reach only the scripted forge, a registered repository and a
//! daemon, with the commands tests run against them.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
use tempfile::TempDir;

use crate::support::{
    self,
    daemon::{CommandGroup, DaemonGuard, Detached},
    git::{git, init_repo},
};

/// Global config without overload warnings, which would add a message to
/// each running agent's workspace whenever the runner is short of disk space,
/// memory or CPU. The daemon's unit tests cover the warnings.
pub const NO_OVERLOAD_WARNINGS: &str = "machine.overload.warning.enabled = false\n";

pub struct Base {
    // Fields drop in declaration order: the commands stop while the daemon
    // still answers them, and what left their group stops before the
    // directory goes.
    pub commands: CommandGroup,
    pub daemon: DaemonGuard,
    _detached: Detached,
    pub root: TempDir,
    pub repo: PathBuf,
}

/// A new home under `/tmp` with the scripted forge installed, to prepare
/// before [`Base::start`] takes it.
pub fn home() -> TempDir {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    support::forge::install(root.path());
    root
}

impl Base {
    /// Start a daemon in `root` with `config` after [`NO_OVERLOAD_WARNINGS`]
    /// as the global config, and register repository `name` holding `files`,
    /// whose `main` is on its origin.
    pub fn start(root: TempDir, config: &str, name: &str, files: &[(&str, &str)]) -> Self {
        write_global_config(root.path(), config);
        let repo = offline_repository(root.path(), name, files);
        let daemon = DaemonGuard::start(root.path(), &mut daemon_launch(root.path()));
        let base = Self {
            commands: CommandGroup::new(),
            _detached: Detached::new(root.path().join("state")),
            daemon,
            root,
            repo,
        };
        base.ok(&["repo", "add", base.repo.to_str().unwrap()]);
        base
    }

    pub fn restart(&mut self) {
        self.daemon
            .restart(self.root.path(), &mut daemon_launch(self.root.path()));
    }

    pub fn command(&self) -> Command {
        let mut command = support::cli(self.root.path());
        self.commands.join(&mut command);
        command
    }

    /// What the Shoal command `command` printed once it ended. One still
    /// running after five minutes fails the test, naming it: a hang guard
    /// that outlasts the daemon's own on the tools it runs.
    pub fn output(&self, command: &mut Command) -> Output {
        support::run::output_within(command, Duration::from_secs(300))
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.output(self.command().args(args))
    }

    pub fn ok(&self, args: &[&str]) -> Value {
        self.ok_from(&mut self.command(), args)
    }

    /// The JSON `command` prints with `args`, which must succeed.
    pub fn ok_from(&self, command: &mut Command, args: &[&str]) -> Value {
        let output = self.output(command.arg("--json").args(args));
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    pub fn request(&self, method: Value) -> Value {
        self.request_as(method, None)
    }

    /// The daemon's answer to `method` from a caller with `scope`, or the user.
    pub fn request_as(&self, method: Value, scope: Option<&str>) -> Value {
        let root = self.root.path();
        let protocol = support::daemon_client::protocol(root);
        support::daemon_client::exchange(
            root,
            &json!({"protocol": protocol, "id": 1, "method": method, "scope": scope}),
        )
    }

    /// The ID of the space named `name`, which commands select it by,
    /// since a name never does.
    pub fn id(&self, name: &str) -> String {
        support::daemon_client::space_id(self.root.path(), name)
    }
}

/// Write `config` after [`NO_OVERLOAD_WARNINGS`] as the global config in `root`.
fn write_global_config(root: &Path, config: &str) {
    fs::create_dir_all(root.join(".config/shoal")).unwrap();
    fs::write(
        root.join(".config/shoal/config.toml"),
        format!("{NO_OVERLOAD_WARNINGS}{config}"),
    )
    .unwrap();
}

/// Create repository `name` in `root` holding `files`, whose `main` is on its
/// origin and whose fetches never reach the network.
fn offline_repository(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let repo = init_repo(root, name, files);
    // Model a starting commit already present on a remote, without network I/O.
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    // Fetches over SSH, as the daemon's background sync of a GitHub
    // origin a test adds, fail here without reaching the network; a test
    // that serves SSH itself sets its own command.
    git(&repo, &["config", "core.sshCommand", "false"]);
    repo
}

/// How fixtures start their daemon: as the CLI runs, with a `zmx` on the
/// daemon's PATH when the runner has none.
fn daemon_launch(root: &Path) -> Command {
    let mut launch = support::cli(root);
    support::daemon::provide_zmx(root, &mut launch);
    launch
}
