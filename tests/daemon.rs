use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tempfile::TempDir;

struct Daemon {
    root: TempDir,
    child: Child,
}

impl Daemon {
    fn start() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let child = command(root.path())
            .args(["daemon", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut daemon = Self { root, child };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if command(daemon.root.path())
                .args(["daemon", "status"])
                .output()
                .unwrap()
                .status
                .success()
            {
                break;
            }
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "daemon exited during startup"
            );
            assert!(Instant::now() < deadline, "daemon startup timed out");
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn run(&self, args: &[&str]) -> Output {
        command(self.root.path()).args(args).output().unwrap()
    }

    fn socket(&self) -> UnixStream {
        let stream = UnixStream::connect(self.root.path().join("state/daemon.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
    }

    fn request(&self, request: &Value) -> Value {
        let mut stream = self.socket();
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shoal"));
    command
        .arg("--state-dir")
        .arg(root.join("state"))
        .env("HOME", root)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("SHOAL_SCOPE_TOKEN")
        .env_remove("SHOAL_EXECUTION_ID");
    command
}

#[test]
fn cli_connects_to_daemon_and_stops_it() {
    let mut daemon = Daemon::start();
    let output = daemon.run(&["--json", "daemon", "status"]);
    assert!(output.status.success());
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["daemon"]["pid"], daemon.child.id());
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
    assert!(daemon.child.wait().unwrap().success());
    assert!(!daemon.root.path().join("state/daemon.sock").exists());
    let output = daemon.run(&["--json", "daemon", "status"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["running"],
        false
    );
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
    let mut line = String::new();
    BufReader::new(oversized).read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["data"]["code"],
        "invalid_request"
    );
    assert!(daemon.run(&["daemon", "status"]).status.success());
}

#[test]
fn socket_is_recovered_after_abrupt_exit() {
    let mut daemon = Daemon::start();
    daemon.child.kill().unwrap();
    daemon.child.wait().unwrap();
    assert!(daemon.root.path().join("state/daemon.sock").exists());
    daemon.child = command(daemon.root.path())
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !daemon.run(&["daemon", "status"]).status.success() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
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

#[test]
fn install_preview_does_not_install_a_service() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let output = command(root.path())
        .args(["--json", "install", "--dry-run"])
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
        fs::create_dir_all(root.join("state")).unwrap();
        let listener = UnixListener::bind(root.join("state/daemon.sock")).unwrap();
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
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                if malformed {
                    writeln!(stream, "not json").unwrap();
                } else {
                    let response = json!({
                        "protocol": request["protocol"].as_u64().unwrap() + 1,
                        "id": request["id"],
                        "type": "error",
                        "data": {"code": "protocol_mismatch", "message": "old daemon"}
                    });
                    writeln!(stream, "{response}").unwrap();
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
    let output = command(root.path()).arg("install").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("stop the foreground daemon"));
    assert!(
        command(root.path())
            .args(["install", "--dry-run"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn install_preserves_a_compatible_foreground_daemon() {
    let daemon = Daemon::start();
    let output = daemon.run(&["install"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("foreground daemon"));
    assert!(daemon.run(&["daemon", "status"]).status.success());
}

#[test]
fn install_is_repeatable_and_service_controls_work_with_an_isolated_manager() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    // Emulate only the OS command boundary. The installed definition and daemon
    // are real; no commands reach this machine's launchd/systemd instance.
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
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let pid_path = root.path().join("pid");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(pid) = fs::read_to_string(&self.0) {
                // PID comes only from the process spawned by the test manager.
                if let Ok(pid) = pid.trim().parse::<i32>() {
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
        }
    }
    let _cleanup = Cleanup(pid_path.clone());
    let run = |args: &[&str]| {
        let output = command(root.path())
            .args(args)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("SHOAL_BINARY", env!("CARGO_BIN_EXE_shoal"))
            .env("SHOAL_TEST_STATE", root.path().join("state"))
            .env("FAKE_PID", &pid_path)
            .env("FAKE_LOG", root.path().join("service.log"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    let output: Value = serde_json::from_slice(&run(&["--json", "install"]).stdout).unwrap();
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
    let template = config.with_file_name("issue-template.md");
    assert_eq!(
        fs::read_to_string(&template).unwrap(),
        include_str!("../issue-template.md")
    );
    fs::write(&template, "custom issue template").unwrap();
    let agent_template = config.with_file_name("agent-template.md");
    assert_eq!(
        fs::read_to_string(&agent_template).unwrap(),
        include_str!("../agent-template.md")
    );
    fs::write(&agent_template, "custom agent template").unwrap();
    fs::write(&config, "default_agent = 'claude'\n").unwrap();
    let output: Value = serde_json::from_slice(&run(&["--json", "install"]).stdout).unwrap();
    assert_eq!(output["config_created"], false);
    assert_eq!(
        fs::read_to_string(agent_template).unwrap(),
        "custom agent template"
    );
    assert_eq!(
        fs::read_to_string(&template).unwrap(),
        "custom issue template"
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
    let repo = root.path().join("repo");
    fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    run(&["repo", "add", repo.to_str().unwrap()]);
    run(&["add", repo.to_str().unwrap(), "--name", "keep-running"]);
    let marker = root.path().join("started");
    let finish = root.path().join("finish");
    let mut execution = command(root.path())
        .args([
            "exec",
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            execution.try_wait().unwrap().is_none(),
            "execution exited before install"
        );
        assert!(Instant::now() < deadline, "execution did not start");
        thread::sleep(Duration::from_millis(20));
    }
    let replacement = bin.join("updated-shoal");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_shoal"), &replacement).unwrap();
    run(&["install", "--executable", replacement.to_str().unwrap()]);
    assert_eq!(fs::read_to_string(&pid_path).unwrap(), original);
    let preview: Value =
        serde_json::from_slice(&run(&["--json", "install", "--dry-run"]).stdout).unwrap();
    assert!(
        fs::read_to_string(preview["path"].as_str().unwrap())
            .unwrap()
            .contains(replacement.to_str().unwrap())
    );
    let inspection: Value =
        serde_json::from_slice(&run(&["--json", "inspect", "keep-running"]).stdout).unwrap();
    assert_eq!(inspection["executions"][0]["state"], "running");
    assert!(execution.try_wait().unwrap().is_none());
    fs::write(&finish, "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = execution.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "execution did not finish");
        thread::sleep(Duration::from_millis(20));
    }

    // Replace only the wire response. Stopping the real managed daemon removes
    // its socket, allowing install to register and start the current binary.
    let socket = root.path().join("state/daemon.sock");
    let hidden_socket = root.path().join("state/managed.sock");
    fs::rename(&socket, &hidden_socket).unwrap();
    {
        let _daemon = IncompatibleDaemon::listen(root.path(), true);
        let output = command(root.path())
            .arg("install")
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
        run(&["--json", "install"]);
    }
    assert_ne!(fs::read_to_string(&pid_path).unwrap(), original);
    let upgraded = fs::read_to_string(&pid_path).unwrap();
    run(&["install"]);
    assert_eq!(fs::read_to_string(&pid_path).unwrap(), upgraded);
    run(&["daemon", "restart"]);
    assert_ne!(fs::read_to_string(&pid_path).unwrap(), original);
    run(&["daemon", "stop"]);
    run(&["daemon", "stop"]);
    run(&["daemon", "start"]);
    run(&["daemon", "stop"]);
}
