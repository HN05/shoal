#[path = "support/daemon.rs"]
mod daemon_fixture;
use daemon_fixture::DaemonGuard;

#[path = "support/git.rs"]
mod git_fixture;

#[path = "support/prompt.rs"]
mod prompt;
#[path = "support/pty.rs"]
mod pty;
mod support;

use std::{
    fs,
    io::Write,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::{Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use support::{
    cli as command, daemon_client,
    wait::{wait_for, wait_until},
};

use serde_json::{Value, json};
use tempfile::TempDir;

struct Daemon {
    guard: DaemonGuard,
    root: TempDir,
}

impl Daemon {
    fn start() -> Self {
        Self::with_path(None)
    }

    fn with_path(path: Option<&Path>) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let mut launch = command(root.path());
        if let Some(path) = path {
            launch.env("PATH", path);
        }
        let guard = DaemonGuard::start(root.path(), &mut launch);
        Self { root, guard }
    }

    fn run(&self, args: &[&str]) -> Output {
        command(self.root.path()).args(args).output().unwrap()
    }

    fn socket(&self) -> UnixStream {
        daemon_client::connect(self.root.path())
    }

    fn request(&self, request: &Value) -> Value {
        daemon_client::exchange(self.root.path(), request)
    }
}

#[test]
fn daemon_guard_reaps_child_when_a_test_panics() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let pid = std::cell::Cell::new(None);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let daemon = DaemonGuard::start(root.path(), &mut command(root.path()));
        pid.set(Some(daemon.child.id()));
        panic!("simulate a failed test assertion");
    }));
    assert!(result.is_err());
    let mut status = 0;
    // A reaped child is no longer waitable; a running or zombie child is.
    assert_eq!(
        unsafe { libc::waitpid(pid.get().unwrap() as i32, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

/// A failed test shows how its daemon ended and everything it logged.
#[test]
fn daemon_guard_reports_the_daemon_exit_and_log() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut daemon = DaemonGuard::start(root.path(), &mut command(root.path()));
    daemon.child.kill().unwrap();
    daemon.child.wait().unwrap();
    let report = daemon.report();
    assert!(report.starts_with("daemon exited (signal: 9"), "{report}");
    assert!(report.contains("shoal daemon listening on"), "{report}");
}

#[test]
fn cli_connects_to_daemon_and_stops_it() {
    let mut daemon = Daemon::start();
    let output = daemon.run(&["--json", "daemon", "status"]);
    assert!(output.status.success());
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["daemon"]["pid"], daemon.guard.child.id());
    assert_eq!(status["running"], true);
    assert_eq!(
        fs::metadata(daemon.root.path().join("state/daemon.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let stopped = daemon.run(&["daemon", "stop"]);
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(daemon.guard.child.wait().unwrap().success());
    assert!(!daemon.root.path().join("state/daemon.sock").exists());
    let output = daemon.run(&["--json", "daemon", "status"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["running"],
        false
    );
}

#[test]
fn managed_update_preserves_pid_ownership_and_queued_requests() {
    use std::os::unix::fs::{MetadataExt, symlink};
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let launcher = root.path().join("shoal");
    let gate = root.path().join("replacement-started");
    let replacement = root.path().join("replacement");
    symlink(env!("CARGO_BIN_EXE_shoal"), &launcher).unwrap();
    support::install_test_script(
        &replacement,
        r#"#!/bin/sh
printf ready > "$SHOAL_TEST_UPDATE_GATE"
IFS= read -r resume
exec "$SHOAL_TEST_REAL_BINARY" "$@"
"#,
    );
    let log = tempfile::tempfile().unwrap();
    let mut launch = support::isolated(root.path(), &launcher);
    launch
        .args(["daemon", "run", "--managed"])
        .env("SHOAL_TEST_UPDATE_GATE", &gate)
        .env("SHOAL_TEST_REAL_BINARY", env!("CARGO_BIN_EXE_shoal"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(log.try_clone().unwrap());
    let mut daemon = DaemonGuard {
        child: launch.spawn().unwrap(),
        stderr: Some(log),
    };
    wait_until("daemon startup", || {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "daemon exited during startup"
        );
        command(root.path())
            .args(["daemon", "status"])
            .output()
            .unwrap()
            .status
            .success()
    });
    let socket = root.path().join("state/daemon.sock");
    let before = fs::metadata(&socket).unwrap();
    let protocol = daemon_client::protocol(root.path());
    let next = root.path().join("shoal-next");
    symlink(&replacement, &next).unwrap();
    fs::rename(next, &launcher).unwrap();
    wait_until("replacement to start", || {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "daemon exited before handoff"
        );
        gate.exists()
    });
    // The shim pauses between the two executables. The old daemon's socket and
    // lock must still be live, although neither daemon can serve requests yet.
    let other = command(root.path())
        .args(["daemon", "run", "--managed"])
        .output()
        .unwrap();
    assert!(!other.status.success());
    assert!(String::from_utf8_lossy(&other.stderr).contains("a daemon already owns"));
    let queued = daemon_client::connect(root.path());
    daemon_client::send(
        &queued,
        &json!({"protocol":protocol,"id":42,"method":"status"}),
    );
    writeln!(daemon.child.stdin.as_mut().unwrap(), "resume").unwrap();
    let response = daemon_client::receive(&queued);
    assert_eq!(response["id"], 42);
    assert_eq!(response["data"]["pid"], daemon.child.id());
    let after = fs::metadata(socket).unwrap();
    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
}

#[test]
fn managed_daemon_restarts_for_a_replacement_during_startup() {
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{OpenOptionsExt, symlink},
    };
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let launcher = root.path().join("shoal");
    let started = root.path().join("replacement-started");
    let replacement = root.path().join("replacement");
    symlink(env!("CARGO_BIN_EXE_shoal"), &launcher).unwrap();
    support::install_test_script(
        &replacement,
        r#"#!/bin/sh
printf ready > "$SHOAL_TEST_UPDATE_GATE"
exec "$SHOAL_TEST_REAL_BINARY" "$@"
"#,
    );
    // Startup blocks reading the global config until the test writes it.
    let config = root.path().join(".config/shoal/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let fifo = std::ffi::CString::new(config.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let mut launch = support::isolated(root.path(), &launcher);
    launch
        .args(["daemon", "run", "--managed"])
        .env("SHOAL_TEST_UPDATE_GATE", &started)
        .env("SHOAL_TEST_REAL_BINARY", env!("CARGO_BIN_EXE_shoal"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut daemon = DaemonGuard {
        child: launch.spawn().unwrap(),
        stderr: None,
    };
    // A nonblocking writer opens only once the daemon is reading.
    let mut writer = None;
    wait_until("daemon to read its config", || {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "daemon exited during startup"
        );
        writer = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&config)
            .ok();
        writer.is_some()
    });
    // The restarted daemon reads a regular file.
    let settled = root.path().join("config.toml");
    fs::write(&settled, "").unwrap();
    fs::rename(&settled, &config).unwrap();
    let next = root.path().join("shoal-next");
    symlink(&replacement, &next).unwrap();
    fs::rename(next, &launcher).unwrap();
    drop(writer);
    wait_until("replacement to start", || {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "daemon exited instead of restarting"
        );
        started.exists()
    });
    wait_until("restarted daemon", || {
        command(root.path())
            .args(["daemon", "status"])
            .output()
            .unwrap()
            .status
            .success()
    });
}

#[test]
fn global_config_changes_reload_a_running_daemon() {
    let daemon = Daemon::start();
    let policy = || {
        let output = daemon.run(&["--json", "leases", "sim", "--all"]);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["policy"].clone()
    };
    assert!(policy()["default"].is_null());
    let output = daemon.run(&[
        "--json",
        "config",
        "set",
        "simulators.profiles.phone.device",
        "iPhone 17",
        "simulators.profiles.phone.runtime",
        "iOS 27",
        "simulators.default",
        "phone",
    ]);
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["daemon_reloaded"], true);
    assert_eq!(policy()["default"], "phone");
    // Hand edits apply on an explicit reload; an invalid file keeps the running config.
    let config = daemon.root.path().join(".config/shoal/config.toml");
    fs::write(&config, "[simulators]\nmax_booted = 0\n").unwrap();
    assert!(!daemon.run(&["daemon", "reload"]).status.success());
    assert_eq!(policy()["default"], "phone");
    fs::write(&config, "").unwrap();
    assert!(daemon.run(&["daemon", "reload"]).status.success());
    assert!(policy()["default"].is_null());
    let scoped = command(daemon.root.path())
        .args(["daemon", "reload"])
        .env("SHOAL_SCOPE_TOKEN", "test-scope")
        .output()
        .unwrap();
    assert!(!scoped.status.success());
}

#[test]
fn second_daemon_cannot_steal_the_socket() {
    let daemon = Daemon::start();
    let output = daemon.run(&["daemon", "run"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already owns"));
    assert!(daemon.run(&["daemon", "status"]).status.success());
}

#[test]
fn protocol_rejects_bad_clients_and_remains_available() {
    let daemon = Daemon::start();
    let response = daemon.request(&json!({"protocol": 999, "id": 7, "method": "status"}));
    assert_eq!(response["id"], 7);
    assert_eq!(response["data"]["code"], "protocol_mismatch");
    let response = daemon.request(&json!({"protocol": 1, "id": 8, "method": "unknown"}));
    assert_eq!(response["data"]["code"], "invalid_request");
    let mut stalled = daemon.socket();
    stalled.write_all(b"{\"protocol\":").unwrap();
    assert!(daemon.run(&["daemon", "status"]).status.success());
    let mut oversized = daemon.socket();
    oversized.write_all(&vec![b'x'; 65537]).unwrap();
    assert_eq!(
        daemon_client::receive(&oversized)["data"]["code"],
        "invalid_request"
    );
    assert!(daemon.run(&["daemon", "status"]).status.success());
}

#[test]
fn socket_is_recovered_after_abrupt_exit() {
    let mut daemon = Daemon::start();
    daemon.guard.child.kill().unwrap();
    daemon.guard.child.wait().unwrap();
    assert!(daemon.root.path().join("state/daemon.sock").exists());
    daemon
        .guard
        .restart(daemon.root.path(), &mut command(daemon.root.path()));
}

#[test]
fn refuses_to_replace_a_regular_file_at_socket_path() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    fs::create_dir(root.path().join("state")).unwrap();
    let socket = root.path().join("state/daemon.sock");
    fs::write(&socket, "keep me").unwrap();
    assert!(
        !command(root.path())
            .args(["daemon", "run"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(socket).unwrap(), "keep me");
}

/// Startup recovery that cannot save its result stops the daemon before it binds.
#[test]
fn failed_startup_recovery_never_serves() {
    // A preparing workspace fails quarantine; a ready one with a missing
    // worktree fails the ownership audit.
    for state in ["preparing", "ready"] {
        let daemon = Daemon::start();
        let stopped = daemon.run(&["daemon", "stop"]);
        assert!(stopped.status.success(), "{stopped:?}");
        let Daemon { mut guard, root } = daemon;
        assert!(guard.child.wait().unwrap().success());
        rusqlite::Connection::open(root.path().join("state/state.db"))
            .unwrap()
            .execute_batch(&format!(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                    VALUES ('workspace','repo','worker','/missing/worker','worker','{state}');
                CREATE TRIGGER block_recovery BEFORE UPDATE OF state ON workspaces
                    BEGIN SELECT RAISE(ABORT, 'recovery blocked'); END;"
            ))
            .unwrap();
        let stderr = tempfile::NamedTempFile::new().unwrap();
        guard.child = command(root.path())
            .args(["daemon", "run"])
            .stdout(Stdio::null())
            .stderr(stderr.reopen().unwrap())
            .spawn()
            .unwrap();
        let socket = root.path().join("state/daemon.sock");
        let status = wait_for("daemon exit", || {
            assert!(!socket.exists(), "{state}: daemon bound its socket");
            guard.child.try_wait().unwrap()
        });
        assert!(!status.success(), "{state}");
        let output = fs::read_to_string(stderr.path()).unwrap();
        assert!(output.contains("recovery blocked"), "{state}: {output}");
        assert!(!socket.exists(), "{state}");
    }
}

#[test]
fn install_preview_does_not_install_a_service() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let output = command(root.path())
        .args(["--json", "daemon", "install", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let definition: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        definition["content"]
            .as_str()
            .unwrap()
            .contains("--managed")
    );
    assert!(!Path::new(definition["path"].as_str().unwrap()).exists());
    assert!(!root.path().join(".config").exists());
    assert!(!root.path().join("state").exists());
}

struct IncompatibleDaemon {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl IncompatibleDaemon {
    fn listen(root: &Path, malformed: bool) -> Self {
        Self::with_version(root, malformed, false)
    }

    fn with_version(root: &Path, malformed: bool, version_mismatch: bool) -> Self {
        let listener = support::fake_daemon::bind_fake_daemon(root);
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let thread = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                let request = support::fake_daemon::read_request(&stream)
                    .expect("missing fake daemon request");
                if malformed {
                    writeln!(stream, "not json").unwrap();
                } else if version_mismatch {
                    let response = json!({
                        "type": "status",
                        "data": {"pid": 123, "version": "0.0.0", "uptime_secs": 1,
                                 "managed": false, "unread_notifications": 0}
                    });
                    support::fake_daemon::write_reply(&stream, &request, response);
                } else {
                    let response = json!({
                        "type": "error",
                        "data": {"code": "protocol_mismatch", "message": "old daemon"}
                    });
                    let newer = request["protocol"].as_u64().unwrap() + 1;
                    let frame =
                        support::fake_daemon::frame_with_protocol(&request, newer, response);
                    stream.write_all(frame.as_bytes()).unwrap();
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for IncompatibleDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn install_refuses_an_incompatible_daemon_without_an_installed_service() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let _daemon = IncompatibleDaemon::listen(root.path(), false);
    let output = command(root.path())
        .args(["daemon", "install"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("stop the foreground daemon"));
    assert!(
        command(root.path())
            .args(["daemon", "install", "--dry-run"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn install_preserves_a_compatible_foreground_daemon() {
    let daemon = Daemon::start();
    let output = daemon.run(&["daemon", "install"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("foreground daemon"));
    assert!(daemon.run(&["daemon", "status"]).status.success());
}

/// launchctl and systemctl stand-ins that run a real managed daemon. Only the
/// OS command boundary is emulated: the installed definition and daemon are
/// real, and no commands reach this machine's launchd/systemd instance.
struct FakeServiceManager {
    root: TempDir,
    bin: std::path::PathBuf,
    pid: std::path::PathBuf,
}

impl FakeServiceManager {
    fn new() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let script = r#"#!/bin/sh
if [ "$1" = --user ]; then shift; fi
start() {
  if [ -f "$FAKE_PID" ]; then return; fi
  "$SHOAL_BINARY" --state-dir "$SHOAL_TEST_STATE" daemon run --managed </dev/null >"$FAKE_LOG" 2>&1 &
  echo $! >"$FAKE_PID"
}
stop() {
  if [ -f "$FAKE_PID" ]; then
    kill "$(cat "$FAKE_PID")"
    rm "$FAKE_PID"
  fi
}
case "$1" in
  print) test -f "$FAKE_PID" ;;
  bootstrap|kickstart|start) start ;;
  bootout|stop) stop ;;
  restart) stop; start ;;
  enable|daemon-reload) exit 0 ;;
  *) exit 2 ;;
esac
"#;
        for name in ["launchctl", "systemctl"] {
            let path = bin.join(name);
            support::install_test_script_with_mode(&path, script, 0o700);
        }
        let pid = root.path().join("pid");
        Self { root, bin, pid }
    }

    fn command(&self) -> std::process::Command {
        let mut command = command(self.root.path());
        command
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("SHOAL_BINARY", env!("CARGO_BIN_EXE_shoal"))
            .env("SHOAL_TEST_STATE", self.root.path().join("state"))
            .env("FAKE_PID", &self.pid)
            .env("FAKE_LOG", self.root.path().join("service.log"));
        command
    }
}

impl Drop for FakeServiceManager {
    fn drop(&mut self) {
        if let Ok(pid) = fs::read_to_string(&self.pid) {
            // PID comes only from the process spawned by the test manager.
            if let Ok(pid) = pid.trim().parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }
}

#[test]
fn daemon_start_offers_to_install_a_missing_service() {
    let manager = FakeServiceManager::new();
    let (output, transcript) = prompt::answer(
        manager.command().args(["daemon", "start"]),
        "n\n",
        prompt::AnswerOptions::default(),
    );
    assert!(!output.status.success(), "{transcript}");
    assert!(
        transcript.contains("Install it with `shoal daemon install`? [y/N]"),
        "{transcript}"
    );
    assert!(
        transcript.contains("daemon service is not installed"),
        "{transcript}"
    );
    assert!(!manager.pid.exists());
    let (output, transcript) = prompt::answer(
        manager.command().args(["daemon", "start"]),
        "y\n",
        prompt::AnswerOptions::default(),
    );
    assert!(output.status.success(), "{transcript}");
    assert!(manager.pid.exists());
    let status = manager
        .command()
        .args(["daemon", "status"])
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
}

#[test]
fn install_only_shows_shell_hint_without_startup_initialization() {
    let manager = FakeServiceManager::new();
    let check_hint = |expected, zdotdir: &std::path::Path| {
        let output = manager
            .command()
            .env("ZDOTDIR", zdotdir)
            .args(["daemon", "install"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.contains("Add this line"), expected, "{text}");
    };
    let home = manager.root.path();
    check_hint(true, home);
    for (file, contents, expected) in [
        (".bashrc", "# source <(shoal shell init)\n", true),
        (".zshrc", "echo 'source <(shoal shell init)'\n", true),
        (".bashrc", "source <(shoal shell init)\n", false),
        (".zshrc", "  source <(shoal  shell\tinit) # Shoal\n", false),
        (".bashrc", "eval \"$(shoal shell init)\"\n", false),
        (".zshrc", "source <(command shoal shell init);\n", false),
        (".zshrc", "eval \"$(command shoal shell init)\"\n", false),
    ] {
        let path = home.join(file);
        fs::write(&path, contents).unwrap();
        check_hint(expected, home);
        fs::remove_file(path).unwrap();
    }
    let zdotdir = home.join("zsh-config");
    fs::create_dir(&zdotdir).unwrap();
    fs::write(home.join(".zshrc"), "source <(shoal shell init)\n").unwrap();
    check_hint(true, &zdotdir);
    fs::write(zdotdir.join(".zshrc"), "source <(shoal shell init)\n").unwrap();
    check_hint(false, &zdotdir);
    // An unreadable startup file must not turn a successful install into failure.
    fs::remove_file(zdotdir.join(".zshrc")).unwrap();
    fs::create_dir(zdotdir.join(".zshrc")).unwrap();
    check_hint(true, &zdotdir);
}

#[test]
fn install_is_repeatable_and_service_controls_work_with_an_isolated_manager() {
    let manager = FakeServiceManager::new();
    let root = &manager.root;
    let bin = manager.bin.clone();
    let pid_path = manager.pid.clone();
    let run = |args: &[&str]| {
        let output = manager.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    let output: Value =
        serde_json::from_slice(&run(&["--json", "daemon", "install"]).stdout).unwrap();
    let original = fs::read_to_string(&pid_path).unwrap();
    // A missing global config is seeded once with the defaults, then left alone.
    let config = root.path().join(".config/shoal/config.toml");
    assert_eq!(output["config"], config.to_str().unwrap());
    assert_eq!(output["config_created"], true);
    assert!(
        fs::read_to_string(&config)
            .unwrap()
            .contains("# default_agent = \"codex\"")
    );
    let template = config.with_file_name("chat-template.md");
    assert_eq!(
        fs::read_to_string(&template).unwrap(),
        include_str!("../src/config/templates/chat.md")
    );
    assert!(!config.with_file_name("issue-template.md").exists());
    fs::write(&template, "custom chat template").unwrap();
    // Shoal's agent guidance is built in; the file holds only additions.
    assert!(!config.with_file_name("worker-template.md").exists());
    fs::write(&config, "default_agent = 'claude'\n").unwrap();
    let output: Value =
        serde_json::from_slice(&run(&["--json", "daemon", "install"]).stdout).unwrap();
    assert_eq!(output["config_created"], false);
    assert_eq!(
        fs::read_to_string(&template).unwrap(),
        "custom chat template"
    );
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "default_agent = 'claude'\n"
    );
    assert_eq!(fs::read_to_string(&pid_path).unwrap(), original);
    // `config reset` needs no confirmation: the edited file becomes the backup.
    let output: Value =
        serde_json::from_slice(&run(&["--json", "config", "reset"]).stdout).unwrap();
    let backup = root.path().join(".config/shoal/config.toml.backup");
    assert_eq!(output["backup"], backup.to_str().unwrap());
    assert_eq!(
        fs::read_to_string(&backup).unwrap(),
        "default_agent = 'claude'\n"
    );
    assert!(
        fs::read_to_string(&config)
            .unwrap()
            .contains("# default_agent = \"codex\"")
    );

    // A changed executable path must update the definition without breaking an
    // existing execution's daemon connection.
    let repo = git_fixture::init_repo(root.path(), "repo", &[]);
    run(&["repo", "add", repo.to_str().unwrap()]);
    run(&["add", repo.to_str().unwrap(), "keep-running"]);
    let marker = root.path().join("started");
    let finish = root.path().join("finish");
    let mut execution = command(root.path())
        .args([
            "run",
            "--workspace",
            "keep-running",
            "--",
            "sh",
            "-c",
            "touch \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.05; done",
            "sh",
        ])
        .arg(&marker)
        .arg(&finish)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("execution to start", || {
        assert!(
            execution.try_wait().unwrap().is_none(),
            "execution exited before install"
        );
        marker.exists()
    });
    let replacement = bin.join("updated-shoal");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_shoal"), &replacement).unwrap();
    run(&[
        "daemon",
        "install",
        "--executable",
        replacement.to_str().unwrap(),
    ]);
    assert_eq!(fs::read_to_string(&pid_path).unwrap(), original);
    let preview: Value =
        serde_json::from_slice(&run(&["--json", "daemon", "install", "--dry-run"]).stdout).unwrap();
    assert!(
        fs::read_to_string(preview["path"].as_str().unwrap())
            .unwrap()
            .contains(replacement.to_str().unwrap())
    );
    let inspection: Value =
        serde_json::from_slice(&run(&["--json", "work", "inspect", "keep-running"]).stdout)
            .unwrap();
    assert_eq!(inspection["executions"][0]["state"], "running");
    assert!(execution.try_wait().unwrap().is_none());
    fs::write(&finish, "").unwrap();
    let status = wait_for("execution to finish", || execution.try_wait().unwrap());
    assert!(status.success());

    // Replace only the wire response. Stopping the real managed daemon removes
    // its socket, allowing install to register and start the current binary.
    let socket = root.path().join("state/daemon.sock");
    let hidden_socket = root.path().join("state/managed.sock");
    fs::rename(&socket, &hidden_socket).unwrap();
    {
        let _daemon = IncompatibleDaemon::listen(root.path(), true);
        let output = command(root.path())
            .args(["daemon", "install"])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Updating daemon"));
        assert_eq!(fs::read_to_string(&pid_path).unwrap(), original);
    }
    fs::remove_file(&socket).unwrap();
    {
        let _daemon = IncompatibleDaemon::listen(root.path(), false);
        run(&["--json", "daemon", "install"]);
    }
    assert_ne!(fs::read_to_string(&pid_path).unwrap(), original);
    let upgraded = fs::read_to_string(&pid_path).unwrap();
    run(&["daemon", "install"]);
    assert_eq!(fs::read_to_string(&pid_path).unwrap(), upgraded);
    run(&["daemon", "restart"]);
    assert_ne!(fs::read_to_string(&pid_path).unwrap(), original);
    run(&["daemon", "stop"]);
    run(&["daemon", "stop"]);
    run(&["daemon", "start"]);
    run(&["daemon", "stop"]);
}

fn doctor_report(root: &Path) -> Value {
    let output = command(root)
        .args(["--json", "doctor", "--all", "--repair"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn doctor_reports_a_stopped_daemon_without_creating_state() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let report = doctor_report(root.path());
    assert!(
        report["checks"][0]["message"]
            .as_str()
            .unwrap()
            .contains("stopped or unreachable")
    );
    let skipped: Vec<_> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["status"] == "skipped")
        .map(|check| check["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        skipped,
        [
            "dependency:git",
            "dependency:wt",
            "dependency:lsof",
            "dependency:zmx",
            "dependency:fzf",
            "cleanup",
            "root_dir",
            "checkout:*",
            "forge:*",
            "worktrees:*",
            "workspaces"
        ]
    );
    assert_eq!(report["checks"][1]["status"], "skipped");
    assert_eq!(report["workspaces"], json!([]));
    assert!(!root.path().join("state").exists());
}

#[test]
fn doctor_reports_incompatible_unresponsive_and_wrong_version_daemons() {
    for malformed in [false, true] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let _daemon = IncompatibleDaemon::listen(root.path(), malformed);
        let report = doctor_report(root.path());
        assert_eq!(report["checks"][0]["status"], "error");
        assert_eq!(report["checks"][1]["status"], "skipped");
    }
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let _daemon = IncompatibleDaemon::with_version(root.path(), false, true);
    let report = doctor_report(root.path());
    assert!(
        report["checks"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Daemon version 0.0.0 differs")
    );
    assert_eq!(report["checks"][1]["status"], "skipped");

    let root = tempfile::tempdir_in("/tmp").unwrap();
    fs::create_dir(root.path().join("state")).unwrap();
    let _listener = UnixListener::bind(root.path().join("state/daemon.sock")).unwrap();
    let report = doctor_report(root.path());
    assert!(
        report["checks"][0]["message"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
}

#[test]
fn doctor_checks_the_daemon_path_even_when_the_cli_has_dependencies() {
    let empty = tempfile::tempdir().unwrap();
    let daemon = Daemon::with_path(Some(empty.path()));
    let report = doctor_report(daemon.root.path());
    assert_eq!(report["checks"][0]["status"], "ok");
    for name in ["git", "wt", "lsof", "zmx", "fzf"] {
        let check = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == format!("dependency:{name}"))
            .unwrap();
        assert_eq!(
            check["status"],
            if name == "fzf" { "warning" } else { "error" }
        );
    }
}

#[test]
fn cli_typed_response_errors_preserve_output_and_exit_contracts() {
    for (args, body, expected) in [
        (
            vec!["repo", "add", "/unused-repository"],
            json!({"type": "ok"}),
            "expected Repository, received Ok",
        ),
        (
            vec!["repo", "ls"],
            json!({"type": "ok"}),
            "expected Repositories, received Ok",
        ),
        (
            vec!["alerts", "--follow"],
            json!({"type": "workspaces", "data": []}),
            "expected Ok, received Workspaces",
        ),
        (
            vec!["alerts", "--follow"],
            json!({"type": "error", "data": {"code": "scope_denied", "message": "denied"}}),
            "scope_denied: denied",
        ),
        (
            vec!["--json", "repo", "ls"],
            json!({"type": "error", "data": {"code": "operation_failed", "message": "failed: detail"}}),
            "failed: detail",
        ),
        (
            vec!["run", "--workspace", "worker", "--", "true"],
            json!({"type": "error", "data": {"code": "execution_failed", "message": "failed: detail"}}),
            "execution_failed: failed: detail",
        ),
        (
            vec!["--json", "run", "--workspace", "worker", "--", "true"],
            json!({"type": "error", "data": {"code": "future_code", "message": "failed: detail"}}),
            "future_code: failed: detail",
        ),
    ] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let listener = support::fake_daemon::bind_fake_daemon(root.path());
        listener.set_nonblocking(true).unwrap();
        let daemon = thread::spawn(move || {
            // `run` first lists workspaces to remember the one it runs in.
            loop {
                let stream = wait_for("client connection", || match listener.accept() {
                    Ok((stream, _)) => Some(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
                    Err(error) => panic!("{error}"),
                });
                let request = support::fake_daemon::answer(stream, |request| {
                    if request["method"] == "list_workspaces" {
                        json!({"type": "workspaces", "data": []})
                    } else {
                        body.clone()
                    }
                });
                if request["method"] != "list_workspaces" {
                    break;
                }
            }
        });
        let output = command(root.path()).args(&args).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.stdout.is_empty(), "{args:?}");
        if args.contains(&"--json") {
            assert_eq!(
                serde_json::from_str::<Value>(&stderr).unwrap(),
                json!({"error": {"code": "command_failed", "message": expected}})
            );
        } else {
            assert!(stderr.contains(expected), "{args:?}: {stderr}");
        }
    }
}

#[test]
fn event_cli_preserves_adjacent_frames_and_finishes_an_empty_listing() {
    for count in [0, 205] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let listener = support::fake_daemon::bind_fake_daemon(root.path());
        let daemon = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request =
                support::fake_daemon::read_request(&stream).expect("missing fake daemon request");
            assert_eq!(
                request["method"]["watch_workspace_events"],
                json!({"since": 0, "follow": false})
            );
            let response = |body| support::fake_daemon::frame(&request, body);
            let mut frames = response(json!({"type": "ok"}));
            for id in 1..=count {
                frames.push_str(&response(json!({"type": "event_item", "data": {
                    "type": "event", "id": id, "created_at": 0, "kind": "ready",
                    "workspace_id": "w", "repository_id": "r", "name": "worker",
                    "path": "/work", "branch": "worker", "cause": null, "error": null
                }})));
            }
            frames.push_str(&response(json!({"type": "ok"})));
            stream.write_all(frames.as_bytes()).unwrap();
        });
        let output = command(root.path())
            .args(["internal", "events", "--json", "--since", "0"])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        let ids: Vec<_> = output
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["id"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(ids, (1..=count).collect::<Vec<_>>());
    }
}

#[test]
fn commands_wait_for_an_older_managed_daemon_to_restart_with_the_update() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let listener = support::fake_daemon::bind_fake_daemon(root.path());
    // The daemon refuses three requests before the update replaces it.
    let daemon = thread::spawn(move || {
        let mut methods = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            let request =
                support::fake_daemon::read_request(&stream).expect("missing fake daemon request");
            methods.push(request["method"].clone());
            let protocol = request["protocol"].as_u64().unwrap();
            let reply = |protocol: u64, body| {
                support::fake_daemon::frame_with_protocol(&request, protocol, body)
            };
            if methods.len() <= 3 {
                let refusal = json!({"type": "error", "data": {
                    "code": "protocol_mismatch",
                    "message": "restart the daemon with the installed version"
                }});
                stream
                    .write_all(reply(protocol - 1, refusal).as_bytes())
                    .unwrap();
                continue;
            }
            let ok = reply(protocol, json!({"type": "ok"}));
            stream.write_all(format!("{ok}{ok}").as_bytes()).unwrap();
            return methods;
        }
    });
    let events = || {
        command(root.path())
            .args(["internal", "events", "--json", "--since", "0"])
            .output()
            .unwrap()
    };
    // Without a service, nothing restarts the older daemon.
    let refused = events();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("protocol mismatch"));
    let definition = command(root.path())
        .args(["--json", "daemon", "install", "--dry-run"])
        .output()
        .unwrap();
    let definition: Value = serde_json::from_slice(&definition.stdout).unwrap();
    let path = Path::new(definition["path"].as_str().unwrap());
    assert!(path.starts_with(root.path()));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, definition["content"].as_str().unwrap()).unwrap();
    let output = events();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Waiting for the daemon to restart"));
    let methods = daemon.join().unwrap();
    assert_eq!(methods.len(), 4);
    assert!(methods.iter().all(|method| method == &methods[0]));
}
