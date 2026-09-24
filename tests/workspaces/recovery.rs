use crate::support::{Fixture, git, wait_registered_execution};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn doctor_daemon_reports_untracked_worktrees_without_adopting_them() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
    };
    let fixture = Fixture::new();
    let owned = fixture.add("owned");
    let owned_path = Path::new(owned["path"].as_str().unwrap());
    let orphan = owned_path.parent().unwrap().join("nested/orphan");
    let outside = fixture.root.path().join("outside");
    for (path, branch) in [(&orphan, "orphan"), (&outside, "outside")] {
        git(
            &fixture.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
    }
    fs::write(orphan.join("dirty"), "keep me").unwrap();
    let call = |request: Value| {
        let mut socket =
            UnixStream::connect(fixture.root.path().join("state/daemon.sock")).unwrap();
        writeln!(socket, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let protocol =
        call(serde_json::json!({"protocol":0,"id":1,"method":"status"}))["protocol"].clone();
    let response = call(serde_json::json!({"protocol":protocol,"id":2,"method":"diagnose"}));
    assert_eq!(response["type"], "diagnostics");
    let findings: Vec<_> = response["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"].as_str().unwrap().starts_with("worktrees:"))
        .collect();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["status"], "warning");
    assert!(
        findings[0]["message"]
            .as_str()
            .unwrap()
            .contains(orphan.to_str().unwrap())
    );
    assert_eq!(fs::read_to_string(orphan.join("dirty")).unwrap(), "keep me");
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

fn repaired_workspaces(fixture: &Fixture, args: &[&str]) -> Value {
    let reports = recovery_report(fixture, args);
    assert!(
        reports
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["issues"].as_array().unwrap().is_empty()),
        "{reports}"
    );
    reports
}

pub(super) fn recovery_report(fixture: &Fixture, args: &[&str]) -> Value {
    let output = fixture.command().arg("--json").args(args).output().unwrap();
    assert!(
        matches!(output.status.code(), Some(0 | 2)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["workspaces"].take()
}

#[test]
fn daemon_startup_quarantines_operations_and_retains_claims_across_restarts() {
    let mut fixture = Fixture::with_tools(Some("[resources.lock]\n"), true);
    let states = [
        "preparing",
        "removing",
        "stopping",
        "reconciling",
        "ready",
        "failed",
    ];
    let workspaces: Vec<_> = states.iter().map(|name| fixture.add(name)).collect();
    let port = fixture.ok(&["port", "acquire", "web", "preparing"]);
    let resource = fixture.ok(&["resource", "acquire", "lock", "preparing"]);
    let owner = workspaces[0]["id"].as_str().unwrap();
    let simulator = serde_json::json!({
        "id": "simulator", "udid": null, "device": "type.Phone", "runtime": "runtime.iOS",
        "workspace_id": owner, "last_workspace_id": owner, "lease_name": "default",
        "reason": null, "state": "creating", "last_used": 1, "error": null
    });
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    for (state, workspace) in states.iter().zip(&workspaces) {
        db.execute(
            "UPDATE workspaces SET state=?1,error='original error' WHERE id=?2",
            rusqlite::params![state, workspace["id"].as_str().unwrap()],
        )
        .unwrap();
    }
    db.execute("INSERT INTO executions(id,workspace_id,state) VALUES ('execution',?1,'running'),('legacy',?1,'unknown')", [owner]).unwrap();
    db.execute(
        "INSERT INTO simulators(id,record) VALUES ('simulator',?1)",
        [simulator.to_string()],
    )
    .unwrap();
    for status in ["requested", "acquired", "busy", "failed", "interrupted"] {
        db.execute("INSERT INTO simulator_clean_requests(request_id,workspace_id,record) VALUES (?1,?2,?3)",
            rusqlite::params![status, owner, serde_json::json!({"status": status, "reason": "preserve audit", "evicted": ["recorded-device"]}).to_string()]).unwrap();
    }
    for _ in 0..2 {
        fixture.restart();
        for (state, workspace) in states.iter().zip(&workspaces) {
            let inspection = fixture.ok(&["inspect", state]);
            let saved = &inspection["workspace"];
            assert_eq!(
                saved["state"],
                if *state == "ready" { "ready" } else { "failed" }
            );
            assert_eq!(saved["git_dir"], workspace["git_dir"]);
            assert_eq!(saved["git_dir_id"], workspace["git_dir_id"]);
            let expected_error = if ["ready", "failed"].contains(state) {
                "original error"
            } else {
                "daemon stopped during workspace operation; inspect before cleanup"
            };
            assert_eq!(saved["error"], expected_error);
            assert!(
                Path::new(saved["path"].as_str().unwrap())
                    .join("tracked")
                    .exists()
            );
            if *state == "preparing" {
                let executions = inspection["executions"].as_array().unwrap();
                assert_eq!(executions.len(), 2);
                assert!(
                    executions
                        .iter()
                        .all(|execution| execution["state"] == "unknown")
                );
            }
        }
        assert_eq!(fixture.ok(&["port", "preparing"])["reserved"][0], port);
        assert_eq!(
            fixture.ok(&["resource", "preparing"])["leases"][0],
            resource
        );
        let saved: String = db
            .query_row("SELECT record FROM simulators", [], |r| r.get(0))
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), simulator);
        let audits = db
            .prepare("SELECT request_id,record FROM simulator_clean_requests")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(audits.len(), 5);
        for (status, record) in audits {
            let expected = if status == "requested" {
                "interrupted"
            } else {
                &status
            };
            assert_eq!(
                serde_json::from_str::<Value>(&record).unwrap(),
                serde_json::json!({
                    "status": expected, "reason": "preserve audit", "evicted": ["recorded-device"]
                })
            );
        }
    }
}

#[test]
fn doctor_repairs_interrupted_state_and_preserves_work_and_leases() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("interrupted");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("uncommitted"), "preserve me").unwrap();
    let port = fixture.ok(&["port", "acquire", "web", "interrupted"]);
    let resource = fixture.ok(&["resource", "acquire", "lock", "interrupted"]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE workspaces SET state='removing' WHERE name='interrupted'",
        [],
    )
    .unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "failed"
    );
    let preview = recovery_report(&fixture, &["doctor", "interrupted"]);
    assert_eq!(preview[0]["directory"], "valid");
    let human = fixture.run(&["doctor", "interrupted"]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("interrupted: failed (valid)"));
    let issue = preview[0]["issues"][0].as_str().unwrap();
    assert!(issue.starts_with(preview[0]["workspace"]["error"].as_str().unwrap()));
    assert!(issue.contains("--repair"));
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "failed"
    );
    db.execute(
        "UPDATE workspaces SET error=NULL WHERE name='interrupted'",
        [],
    )
    .unwrap();
    let preview = recovery_report(&fixture, &["doctor", "interrupted"]);
    assert!(
        preview[0]["issues"][0]
            .as_str()
            .unwrap()
            .contains("--repair")
    );
    let repaired = repaired_workspaces(&fixture, &["doctor", "interrupted", "--repair"]);
    assert_eq!(repaired[0]["workspace"]["state"], "ready");
    assert_eq!(
        fs::read_to_string(path.join("uncommitted")).unwrap(),
        "preserve me"
    );
    assert_eq!(fixture.ok(&["port", "interrupted"])["reserved"][0], port);
    assert_eq!(
        fixture.ok(&["resource", "interrupted"])["leases"][0],
        resource
    );
    assert!(
        repaired_workspaces(&fixture, &["doctor", "interrupted", "--repair"])[0]["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !fixture
            .run(&[
                "exec",
                "interrupted",
                "--",
                env!("CARGO_BIN_EXE_shoal"),
                "doctor",
                "--all",
                "--repair"
            ])
            .status
            .success()
    );
}

#[test]
fn doctor_detects_moved_and_replaced_worktrees_without_deleting_data() {
    let fixture = Fixture::new();
    let workspace = fixture.add("original");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("dirty"), "saved").unwrap();
    let moved = fixture.root.path().join("moved workspace");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            path.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    let report = recovery_report(&fixture, &["doctor", "original", "--repair"]);
    assert_eq!(report[0]["directory"], "moved");
    let human = fixture.run(&["doctor", "original"]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("original: failed (moved)"));
    let preview = recovery_report(&fixture, &["doctor", "original"]);
    assert_eq!(preview[0]["issues"], report[0]["issues"]);
    assert_eq!(preview[0]["issues"].as_array().unwrap().len(), 1);
    assert!(!fixture.run(&["rm", "original"]).status.success());
    assert_eq!(fs::read_to_string(moved.join("dirty")).unwrap(), "saved");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            moved.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    );
    repaired_workspaces(&fixture, &["doctor", "original", "--repair"]);
    // Replace the admin directory at its SAME path, proving pathname checks alone are insufficient.
    let admin = Path::new(workspace["git_dir"].as_str().unwrap());
    let old = fixture.root.path().join("old-admin");
    fs::rename(admin, &old).unwrap();
    fs::create_dir(admin).unwrap();
    for entry in fs::read_dir(&old).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), admin.join(entry.file_name())).unwrap();
        }
    }
    let report = recovery_report(&fixture, &["doctor", "original", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    assert!(
        report[0]["issues"]
            .to_string()
            .contains("metadata was replaced")
    );
    assert!(
        !fixture
            .run(&["rm", "original", "--yes", "--delete-branch"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["exec", "original", "--", "true"])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(path.join("dirty")).unwrap(), "saved");
}

#[test]
fn deleted_worktrees_are_forgotten_with_their_resources_but_moved_ones_are_kept() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\ncapacity = 3\n"));
    let mut paths = Vec::new();
    for name in ["directory-only", "git-removed", "moved"] {
        let workspace = fixture.add(name);
        fixture.ok(&["port", "acquire", "web", name]);
        fixture.ok(&["resource", "acquire", "lock", name]);
        paths.push(PathBuf::from(workspace["path"].as_str().unwrap()));
    }
    fs::remove_dir_all(&paths[0]).unwrap();
    git(
        &fixture.repo,
        &["worktree", "remove", paths[1].to_str().unwrap()],
    );
    let elsewhere = fixture.root.path().join("elsewhere");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            paths[2].to_str().unwrap(),
            elsewhere.to_str().unwrap(),
        ],
    );
    // The startup sweep forgets deleted worktrees without touching moved ones.
    fixture.restart();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let names: Vec<String> = fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap().to_owned())
            .collect();
        if names == ["moved"] {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "deleted worktrees remain: {names:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fixture.ok(&["inspect", "moved"])["workspace"]["state"],
        "failed"
    );
    assert!(!fixture.run(&["rm", "moved"]).status.success());
    assert_eq!(
        fixture
            .ok(&["port", "list", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .ok(&["resource", "list", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let worktrees = git(&fixture.repo, &["worktree", "list", "--porcelain"]);
    for (name, path) in ["directory-only", "git-removed"].iter().zip(&paths) {
        assert!(!git(&fixture.repo, &["rev-parse", &format!("refs/heads/{name}")]).is_empty());
        assert!(!worktrees.contains(path.to_str().unwrap()));
    }
    // Explicit removal of a deleted worktree needs no reconciliation first.
    let workspace = fixture.add("explicit");
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();
    let result = fixture.ok(&["rm", "explicit"]);
    assert_eq!(result["branch_deleted"], false);
    assert!(!git(&fixture.repo, &["rev-parse", "refs/heads/explicit"]).is_empty());
}

#[test]
fn doctor_stops_identity_verified_orphans_after_wrapper_death() {
    let fixture = Fixture::new();
    fixture.add("orphan");
    let mut wrapper = fixture
        .command()
        .args(["exec", "orphan", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let execution = wait_registered_execution(&fixture, "orphan");
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] != "unknown" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let report = recovery_report(&fixture, &["doctor", "orphan", "--repair"]);
    assert!(
        report[0]["executions"][0]["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["pid"] == execution["child"]["pid"])
    );
    assert_eq!(
        fixture.ok(&["inspect", "orphan"])["executions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    repaired_workspaces(
        &fixture,
        &[
            "doctor",
            "orphan",
            "--repair",
            "--stop",
            "--acknowledge-stopped",
        ],
    );
    assert_eq!(
        fixture.ok(&["inspect", "orphan"])["executions"],
        serde_json::json!([])
    );
    assert_eq!(
        fixture.ok(&["inspect", "orphan"])["workspace"]["state"],
        "ready"
    );
}

#[test]
fn doctor_recovers_daemon_crash_and_requires_acknowledgement_for_legacy_records() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("crash");
    let port = fixture.ok(&["port", "acquire", "web", "crash"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "crash", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "crash");
    fixture.restart();
    wrapper.wait().unwrap();
    let report = recovery_report(&fixture, &["doctor", "crash"]);
    assert_eq!(report[0]["executions"][0]["state"], "unknown");
    repaired_workspaces(
        &fixture,
        &["doctor", "crash", "--repair", "--acknowledge-stopped"],
    );
    assert_eq!(fixture.ok(&["port", "crash"])["reserved"][0], port);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO executions(id,workspace_id,state) VALUES ('legacy',?1,'unknown')",
        [workspace["id"].as_str().unwrap()],
    )
    .unwrap();
    let report = recovery_report(&fixture, &["doctor", "crash", "--repair"]);
    assert!(!report[0]["executions"][0]["cleared"].as_bool().unwrap());
    repaired_workspaces(
        &fixture,
        &["doctor", "crash", "--repair", "--acknowledge-stopped"],
    );
    assert_eq!(
        fixture.ok(&["inspect", "crash"])["executions"],
        serde_json::json!([])
    );
}

#[test]
fn doctor_finds_detached_tagged_children_even_after_the_command_exits() {
    let fixture = Fixture::new();
    fixture.add("detached");
    let script = "import os, subprocess, sys; subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)";
    let output = fixture.run(&["exec", "detached", "--", "python3", "-c", script]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("surviving or unverified"));
    let report = recovery_report(&fixture, &["doctor", "detached"]);
    assert!(
        !report[0]["executions"][0]["processes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    repaired_workspaces(
        &fixture,
        &[
            "doctor",
            "detached",
            "--repair",
            "--stop",
            "--acknowledge-stopped",
        ],
    );
    assert_eq!(
        fixture.ok(&["inspect", "detached"])["executions"],
        serde_json::json!([])
    );
}

#[test]
fn manual_removal_stops_recorded_orphans_before_releasing_resources() {
    let fixture = Fixture::with_config(Some("[resources.lock]\n"));
    fixture.add("orphan");
    fixture.ok(&["resource", "acquire", "lock", "orphan"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "orphan", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let execution = wait_registered_execution(&fixture, "orphan");
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] != "unknown" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    fixture.ok(&["rm", "orphan"]);
    assert_eq!(
        fixture.ok(&["resource", "list", "--all"]),
        serde_json::json!([])
    );
    let pid = execution["child"]["pid"].as_u64().unwrap().to_string();
    let output = Command::new("ps")
        .args(["-p", &pid, "-o", "stat="])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(
        status.trim().is_empty() || status.trim().starts_with('Z'),
        "owned child survived removal"
    );
}

#[test]
fn doctor_all_reports_each_workspace_independently() {
    let fixture = Fixture::new();
    fixture.add("healthy");
    let missing = fixture.add("missing");
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    let reports = recovery_report(&fixture, &["doctor", "--all", "--repair"]);
    assert_eq!(reports.as_array().unwrap().len(), 2);
    assert_eq!(reports[0]["workspace"]["name"], "healthy");
    assert_eq!(reports[0]["workspace"]["state"], "ready");
    assert_eq!(reports[1]["workspace"]["state"], "failed");
}

#[test]
fn doctor_preserves_connected_commands_until_stop_is_explicit() {
    let fixture = Fixture::new();
    fixture.add("connected");
    let port = fixture.ok(&["port", "acquire", "web", "connected"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "connected", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "connected");
    let report = repaired_workspaces(&fixture, &["doctor", "connected", "--repair"]);
    assert_eq!(report[0]["executions"][0]["connected"], true);
    assert_eq!(report[0]["executions"][0]["cleared"], false);
    assert!(wrapper.try_wait().unwrap().is_none());
    repaired_workspaces(&fixture, &["doctor", "connected", "--repair", "--stop"]);
    wrapper.wait().unwrap();
    assert_eq!(
        fixture.ok(&["inspect", "connected"])["executions"],
        serde_json::json!([])
    );
    assert_eq!(fixture.ok(&["port", "connected"])["reserved"][0], port);
}

#[test]
fn stopping_disconnected_execution_does_not_hold_up_other_workspaces() {
    let fixture = Fixture::new();
    let orphan = fixture.add("orphan");
    fixture.add("other");
    let root = Path::new(orphan["path"].as_str().unwrap());
    let script = r#"
import pathlib, signal, sys, time
root = pathlib.Path.cwd()
def stopping(*_):
    (root / 'stopping').touch()
    while not (root / 'other-ran').exists():
        time.sleep(0.01)
    (root / 'saw-other').touch()
    sys.exit(0)
signal.signal(signal.SIGTERM, stopping)
(root / 'ready').touch()
time.sleep(30)
"#;
    let mut wrapper = fixture
        .command()
        .args(["exec", "orphan", "--", "python3", "-c", script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "orphan");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("ready").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    while fixture.ok(&["inspect", "orphan"])["executions"][0]["state"] != "unknown" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let mut stopping = fixture
        .command()
        .args(["stop", "orphan"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    while !root.join("stopping").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let marker = root.join("other-ran");
    let output = fixture.run(&["exec", "other", "--", "touch", marker.to_str().unwrap()]);
    assert!(output.status.success());
    stopping.wait().unwrap(); // Recovery may still require acknowledgement of unreadable environments.
    assert!(
        root.join("saw-other").exists(),
        "other workspace was blocked until orphan was forcibly killed"
    );
}
