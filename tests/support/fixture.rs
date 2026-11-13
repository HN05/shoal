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

/// A chat's agent for [`Base::run_chat`]: it runs until its wrapper stops it
/// or exits, or [`Base::end_chat_agent`] ends it.
const CHAT_AGENT: &str = "#!/bin/sh\ntrap 'exit 0' TERM\n\
    while kill -0 \"$PPID\" 2>/dev/null && [ ! -e \"$HOME/exit-$SHOAL_CHAT\" ]; do sleep 1; done\n";

pub struct Base {
    // Fields drop in declaration order: the commands stop while the daemon
    // still answers them, and what left their group stops before the
    // directory goes.
    sessions: Sessions,
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
            sessions: Sessions::default(),
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

    /// Start the chat `name` with `shoal start --mode chat --here` in the
    /// registered checkout, on a terminal of its own, with `args` such as
    /// `--parent`, its agent a fake `claude` that runs until its wrapper
    /// stops or leaves it, and return the `shoal start` process and the
    /// chat's ID once the daemon tracks its agent.
    pub fn run_chat(&self, name: &str, args: &[&str]) -> (std::process::Child, String) {
        self.run_chat_with(name, args, |_| {})
    }

    /// [`Self::run_chat`] with the command `prepare` adjusts, such as with
    /// the terminal host's variables or another checkout to start in.
    pub fn run_chat_with(
        &self,
        name: &str,
        args: &[&str],
        prepare: impl FnOnce(&mut Command),
    ) -> (std::process::Child, String) {
        // Agents already running keep the script they started with.
        support::install_test_script(self.root.path().join("bin/claude"), CHAT_AGENT);
        // Its own session, whose controlling terminal it hands its agent.
        let mut command = support::cli(self.root.path());
        // SAFETY: setsid and the ioctl only change this child's session.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut command, || {
                if libc::setsid() < 0
                    || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.current_dir(&self.repo).args([
            "start", "--mode", "chat", "--here", "--agent", "claude", "--name", name,
        ]);
        prepare(&mut command);
        let (master, slave) = support::pty::open();
        let mut child = command
            .args(args)
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave)
            .spawn()
            .unwrap();
        self.sessions.0.lock().unwrap().push(child.id());
        let transcript = drain(master);
        match support::wait::try_wait_for(|| self.tracked_chat(name)) {
            Some(id) => (child, id),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let shown = String::from_utf8_lossy(&transcript.lock().unwrap()).into_owned();
                panic!("the chat's agent was never tracked: {shown}");
            }
        }
    }

    /// End the agent [`Self::run_chat`] runs in the chat `id` as if it exited
    /// on its own, and wait until its chat no longer runs.
    pub fn end_chat_agent(&self, id: &str) {
        fs::write(self.root.path().join(format!("exit-{id}")), "").unwrap();
        support::wait::wait_until("the chat's agent exited", || {
            let chats = self.request(json!("list_chats"));
            (chats["data"].as_array().unwrap().iter())
                .any(|chat| chat["id"] == id && chat["running"] == false)
        });
    }

    /// The ID of the chat `name`, once the daemon tracks its agent.
    fn tracked_chat(&self, name: &str) -> Option<String> {
        let chats = self.request(json!("list_chats"));
        let chat = (chats["data"].as_array()?.iter()).find(|chat| chat["name"] == name)?;
        let id = chat["id"].as_str()?.to_owned();
        let agents = self.request(json!("list_agents"));
        let tracked = (agents["data"].as_array()?.iter()).any(|agent| agent["space_id"] == id);
        tracked.then_some(id)
    }

    /// The ID of the space named `name`, which commands select it by,
    /// since a name never does.
    pub fn id(&self, name: &str) -> String {
        support::daemon_client::space_id(self.root.path(), name)
    }
}

/// The sessions of the commands [`Base::run_chat`] started, which leave the
/// fixture's group, ended as the fixture goes.
#[derive(Default)]
struct Sessions(std::sync::Mutex<Vec<u32>>);

impl Drop for Sessions {
    fn drop(&mut self) {
        for leader in self.0.lock().unwrap().drain(..) {
            let leader = leader as libc::pid_t;
            // A leader the test reaped may have given its ID to another
            // process, so only an unreaped one names its session still.
            // SAFETY: waitpid with WNOHANG only reads the child's state.
            if unsafe { libc::waitpid(leader, std::ptr::null_mut(), libc::WNOHANG) } < 0 {
                continue;
            }
            // SAFETY: kill only sends a signal.
            unsafe { libc::kill(-leader, libc::SIGKILL) };
        }
    }
}

/// What a process writes to the terminal `master` reads, read in the
/// background until it closes, so the process never blocks on a full one.
fn drain(mut master: fs::File) -> std::sync::Arc<std::sync::Mutex<Vec<u8>>> {
    use std::io::Read;
    let transcript = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = transcript.clone();
    std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => kept.lock().unwrap().extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });
    transcript
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
