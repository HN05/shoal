use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::Path,
    process::{Child, Command, Output, Stdio},
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
fn setup_preview_does_not_install_a_service() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let output = command(root.path())
        .args(["--json", "setup", "--dry-run"])
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
    assert!(!root.path().join("state").exists());
}

#[test]
fn setup_is_repeatable_and_service_controls_work_with_an_isolated_manager() {
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
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
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
    run(&["setup"]);
    let original = fs::read_to_string(&pid_path).unwrap();
    run(&["setup"]);
    assert_eq!(fs::read_to_string(&pid_path).unwrap(), original);
    run(&["daemon", "restart"]);
    assert_ne!(fs::read_to_string(&pid_path).unwrap(), original);
    run(&["daemon", "stop"]);
    run(&["daemon", "stop"]);
    run(&["daemon", "start"]);
    run(&["daemon", "stop"]);
}
