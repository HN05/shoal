//! Demos: restarts, screens, presets and handoffs.
use super::simulators::SIM_CONFIG;
use super::*;

#[test]
fn failed_demo_handoffs_close_the_unchanged_session() -> anyhow::Result<()> {
    failed_demo_handoffs_only_close_the_original_known_session("same")
}

#[test]
fn failed_demo_handoffs_keep_a_session_with_another_pid() -> anyhow::Result<()> {
    failed_demo_handoffs_only_close_the_original_known_session("pid")
}

#[test]
fn failed_demo_handoffs_keep_a_session_created_again() -> anyhow::Result<()> {
    failed_demo_handoffs_only_close_the_original_known_session("created")
}

#[test]
fn failed_demo_handoffs_leave_a_session_that_is_gone() -> anyhow::Result<()> {
    failed_demo_handoffs_only_close_the_original_known_session("gone")
}

#[test]
fn failed_demo_handoffs_keep_a_session_of_unknown_creation() -> anyhow::Result<()> {
    failed_demo_handoffs_only_close_the_original_known_session("unknown")
}

/// Fail a demo's handoff at each stage after the fake zmx changed its session
/// as `after` names, and check that only the session it started is killed.
fn failed_demo_handoffs_only_close_the_original_known_session(after: &str) -> anyhow::Result<()> {
    for stage in ["set", "send"] {
        let fixture = Fixture::new();
        fixture.add("failed");
        let home = fixture.root.path();
        install_test_script(
            home.join("bin/zmx"),
            include_str!("../fixtures/zmx_handoff.py"),
        );
        let output = fixture
            .command()
            .env("HANDOFF_FAIL", stage)
            .env("HANDOFF_AFTER", after)
            .args(["demo", "start", "--workspace", "failed", "--", "true"])
            .output()?;
        assert!(!output.status.success(), "{stage}/{after}: {output:?}");
        let calls: Vec<Vec<String>> = fs::read_to_string(home.join("handoff-calls.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<serde_json::Result<_>>()?;
        let killed = calls
            .iter()
            .any(|call| call.first().is_some_and(|command| command == "kill"));
        assert_eq!(killed, after == "same", "{stage}/{after}: {calls:?}");
        assert_eq!(
            home.join("handoff-session.json").exists(),
            !matches!(after, "same" | "gone")
        );
    }
    Ok(())
}

#[test]
fn demos_restart_until_stopped_and_show_their_screen() {
    let fixture = Fixture::with_config(Some("[cleanup.done]\nenabled = false\n"));
    let workspace = fixture.add("tried");
    let runs = Path::new(workspace["path"].as_str().unwrap()).join("runs");
    // Without zmx or Herdr on PATH the demo runs in the background with a log.
    let bin = fixture.root.path().join("bin");
    let demo = |args: &[&str]| {
        let output = fixture
            .command()
            .env("PATH", fixture.restricted_path(&bin))
            // As Shoal sets it for the agents it launches.
            .env("SHOAL_OUTPUT", "json")
            .arg("--json")
            .arg("demo")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let started = demo(&[
        "start",
        "--workspace",
        "tried",
        "--",
        "sh",
        "-c",
        "echo run >> runs; echo \"output ${SHOAL_OUTPUT:-unset}\"; echo failing; exit 3",
    ]);
    assert_eq!(started["name"], "demo");
    assert_eq!(started["workspace"], "tried");
    // A command that exits at once runs again after a pause.
    wait_until("the demo to restart", || {
        fs::read_to_string(&runs).is_ok_and(|text| text.lines().count() >= 2)
    });
    let listing = demo(&["ls", "--workspace", "tried"]);
    assert_eq!(listing["running"].as_array().unwrap().len(), 1);
    let inbox = fixture.ok(&["inbox"]);
    let demos: Vec<_> = inbox
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] == "demo")
        .collect();
    assert_eq!(demos.len(), 1, "{inbox}");
    assert_eq!(demos[0]["demo"]["name"], "demo");
    let screen = demo(&["screen", "demo", "--workspace", "tried"]);
    let text = screen["screen"].as_str().unwrap();
    assert!(text.contains("failing"), "{text}");
    // The agent's output setting stays out of what the user tries.
    assert!(text.contains("output unset"), "{text}");
    // Starting the name again replaces the demo once the old one ended.
    let replaced = demo(&[
        "start",
        "--workspace",
        "tried",
        "--",
        "sh",
        "-c",
        "echo replaced; exec sleep 600",
    ]);
    assert_ne!(replaced["execution_id"], started["execution_id"]);
    let listing = demo(&["ls", "--workspace", "tried"]);
    assert_eq!(listing["running"].as_array().unwrap().len(), 1);
    assert_eq!(
        listing["running"][0]["execution_id"],
        replaced["execution_id"]
    );
    fixture.ok(&["done", "tried", "--keep-demos"]);
    let listing = demo(&["ls", "--workspace", "tried"]);
    assert_eq!(listing["running"].as_array().unwrap().len(), 1);

    demo(&["stop", "demo", "--workspace", "tried"]);
    wait_until("the demo to stop", || {
        demo(&["ls", "--workspace", "tried"])["running"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    assert!(
        !fixture
            .command()
            .args(["demo", "screen", "demo", "--workspace", "tried"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

/// `shoal --json demo ARGS` without zmx or Herdr on PATH, so the demo runs in
/// the background with a log.
pub(crate) fn demo(fixture: &Fixture, args: &[&str]) -> Value {
    let bin = fixture.root.path().join("bin");
    let output = fixture
        .command()
        .env("PATH", fixture.restricted_path(&bin))
        .arg("--json")
        .arg("demo")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Start the demo `name` of `tried` with `preset`, whose command writes the
/// value of `variable` to `<name>.env` in the workspace and keeps running.
fn start_preset_demo(fixture: &Fixture, name: &str, preset: &str, variable: &str) -> Value {
    let script = format!("echo ${variable} > {name}.env; exec sleep 600");
    demo(
        fixture,
        &[
            "start",
            name,
            "--preset",
            preset,
            "--workspace",
            "tried",
            "--",
            "sh",
            "-c",
            &script,
        ],
    )
}

/// Wait until the demo writes `value` to `<name>.env`, then stop the
/// workspace's demos with `shoal done`.
fn finish_preset_demo(fixture: &Fixture, workspace: &Value, name: &str, value: &str) {
    let written = Path::new(workspace["path"].as_str().unwrap()).join(format!("{name}.env"));
    wait_until("the demo to receive its variable", || {
        fs::read_to_string(&written).is_ok_and(|text| text.trim() == value)
    });
    // Completion stops every demo of the workspace.
    fixture.ok(&["done", "tried"]);
    wait_until("the demos to stop", || {
        demo(fixture, &["ls", "--workspace", "tried"])["running"]
            .as_array()
            .unwrap()
            .is_empty()
    });
}

#[test]
fn web_demos_reserve_a_port_and_pass_it_as_port() {
    let fixture = Fixture::with_config(Some("[cleanup.done]\nenabled = false\n"));
    let workspace = fixture.add("tried");
    // Another reservation exported as PORT leaves the demo its own port.
    fixture.ok(&[
        "acquire",
        "port",
        "other",
        "--workspace",
        "tried",
        "--env",
        "PORT",
    ]);
    let web = start_preset_demo(&fixture, "site", "web", "PORT");
    let port = web["environment"]["PORT"].as_str().unwrap().to_owned();
    let leases = fixture.ok(&["leases", "port", "--workspace", "tried"]);
    assert!(
        leases.to_string().contains(&format!("\"port\":{port}")),
        "{leases}"
    );
    finish_preset_demo(&fixture, &workspace, "site", &port);
}

#[cfg(target_os = "macos")]
#[test]
fn simulator_demos_share_the_workspace_simulator() {
    let config = format!("{SIM_CONFIG}[cleanup.done]\nenabled = false\n");
    let fixture = Fixture::with_tools(Some(&config), true);
    let workspace = fixture.add("tried");
    let app = start_preset_demo(&fixture, "app", "simulator", "SHOAL_SIM_UDID");
    let udid = app["environment"]["SHOAL_SIM_UDID"]
        .as_str()
        .unwrap()
        .to_owned();
    let leases = || fixture.ok(&["leases", "sim", "--workspace", "tried"])["simulators"].clone();
    assert_eq!(leases()[0]["udid"], udid);
    // A second simulator demo runs on the same device.
    let widget = start_preset_demo(&fixture, "widget", "simulator", "SHOAL_SIM_UDID");
    assert_eq!(widget["environment"]["SHOAL_SIM_UDID"], udid);
    finish_preset_demo(&fixture, &workspace, "widget", &udid);
    // The lease stays with the workspace, as one the agent takes does.
    assert_eq!(leases()[0]["workspace_id"], workspace["id"]);
}

#[test]
fn a_demo_outlives_the_command_that_started_it() {
    let fixture = Fixture::new();
    fixture.add("tried");
    let bin = fixture.root.path().join("bin");
    let binary = support::shoal_binary();
    // A tracked command starts the demo, as an agent does, then exits.
    let output = fixture
        .command()
        .env("PATH", fixture.restricted_path(&bin))
        .args([
            "run",
            "--workspace",
            "tried",
            "--",
            binary,
            "--json",
            "demo",
            "start",
        ])
        .args(["--", "sh", "-c", "exec sleep 600"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Its completion scan finds none of its processes in the demo, so the
    // command finishes and the demo keeps running.
    wait_until("the starting command to finish", || {
        fixture.ok(&["work", "status", "tried"])["executions"]
            .as_array()
            .unwrap()
            .len()
            == 1
    });
    let executions = fixture.ok(&["work", "status", "tried"])["executions"].clone();
    assert_eq!(executions[0]["state"], "running", "{executions}");
    assert_eq!(
        demo(&fixture, &["ls", "--workspace", "tried"])["running"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    demo(&fixture, &["stop", "--workspace", "tried"]);
    wait_until("the demo to stop", || {
        demo(&fixture, &["ls", "--workspace", "tried"])["running"]
            .as_array()
            .unwrap()
            .is_empty()
    });
}

#[test]
fn demos_refuse_cleared_scopes_and_leave_no_log_after_removal() {
    let fixture = Fixture::new();
    fixture.add("tried");
    let other = fixture.add("other");
    demo(
        &fixture,
        &[
            "start",
            "--workspace",
            "other",
            "--",
            "sh",
            "-c",
            "echo hello; exec sleep 600",
        ],
    );
    let log = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(other["id"].as_str().unwrap())
        .join("demos/demo.log");
    wait_until("the demo to write its log", || {
        fs::read_to_string(&log).is_ok_and(|text| text.contains("hello"))
    });
    // An invalid name is refused before a workspace is chosen.
    for args in [
        vec!["demo", "stop", "Bad"],
        vec!["demo", "screen", "Bad"],
        vec!["demo", "send", "Bad", "q"],
    ] {
        let refused = fixture.run(&args);
        assert!(!refused.status.success(), "{args:?}");
    }
    // A workspace process without its token still acts only on its own.
    let binary = support::shoal_binary();
    let refused = fixture.run(&[
        "run",
        "--workspace",
        "tried",
        "--",
        "env",
        "-u",
        "SHOAL_SCOPE_TOKEN",
        binary,
        "demo",
        "stop",
        "--workspace",
        "other",
    ]);
    assert!(!refused.status.success());
    assert_eq!(
        demo(&fixture, &["ls", "--workspace", "other"])["running"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.ok(&["rm", "other", "--yes"]);
    assert!(!log.exists());

    // A command that cannot run never reports a started demo.
    let missing = fixture.run(&[
        "demo",
        "start",
        "--workspace",
        "tried",
        "--",
        "/nonexistent/program",
    ]);
    assert!(!missing.status.success());
    assert!(
        demo(&fixture, &["ls", "--workspace", "tried"])["running"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// The process groups of a workspace's only demo: its wrapper's, which a
/// background demo leads, and its command's. Shoal stops neither when the
/// fixture kills the daemon, so a test that ends early kills them here.
struct DemoProcesses {
    groups: [u32; 2],
    ended: bool,
}

impl DemoProcesses {
    fn of(fixture: &Fixture, workspace: &str) -> Self {
        let execution = wait_registered_execution(fixture, workspace);
        let pid = |process: &str| execution[process]["pid"].as_u64().unwrap() as u32;
        Self {
            groups: [pid("wrapper"), pid("child")],
            ended: false,
        }
    }

    /// Wait until the stopped demo's wrapper and command have exited.
    fn await_end(&mut self) {
        wait_until("the demo's processes to exit", || {
            !self.groups.iter().any(|&pid| process_running(pid))
        });
        self.ended = true;
    }
}

impl Drop for DemoProcesses {
    fn drop(&mut self) {
        if !self.ended {
            for group in self.groups {
                unsafe { libc::kill(-(group as i32), libc::SIGKILL) };
            }
        }
    }
}

#[test]
fn a_demo_opener_leaves_the_terminal_as_it_found_it() {
    let fixture = Fixture::new();
    fixture.add("tried");
    demo(
        &fixture,
        &[
            "start",
            "site",
            "--workspace",
            "tried",
            "--open",
            "stty -echo",
            "--",
            "sh",
            "-c",
            "exec sleep 600",
        ],
    );
    let mut processes = DemoProcesses::of(&fixture, "tried");
    let (mut master, slave) = pty::open();
    let mut driver = support::isolated(fixture.root.path(), "python3");
    let mut child = driver
        .env("TERM", "xterm-256color")
        .args([
            "-c",
            r#"import fcntl, os, subprocess, sys, termios
binary = sys.argv[1]
os.setsid()
fcntl.ioctl(0, termios.TIOCSCTTY, 0)
def terminal_settings():
    settings = termios.tcgetattr(0)
    settings[3] &= ~getattr(termios, 'PENDIN', 0)
    return settings
before = terminal_settings()
group = os.tcgetpgrp(0)
opened = subprocess.run([binary, 'demo', 'site', '--workspace', 'tried'], timeout=30)
assert opened.returncode == 0, opened.returncode
assert os.tcgetpgrp(0) == group, 'foreground group was not restored'
assert terminal_settings() == before, (before, terminal_settings())
"#,
            support::shoal_binary(),
        ])
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .spawn()
        .unwrap();
    use std::io::Read;
    let mut transcript = String::new();
    // Drain while the driver runs: closing a PTY can wait for unread output.
    let finished = try_wait_for(|| {
        let _ = master.read_to_string(&mut transcript);
        child.try_wait().unwrap()
    });
    if finished.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("terminal driver hung: {transcript:?}");
    }
    drop(slave);
    let _ = master.read_to_string(&mut transcript);
    assert!(finished.unwrap().success(), "{transcript}");
    // A stop only asks the wrapper, which ends after reporting to the daemon.
    demo(&fixture, &["stop", "--workspace", "tried"]);
    processes.await_end();
}
