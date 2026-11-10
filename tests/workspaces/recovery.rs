//! The daemon's restarts and recovery: reconciling interrupted state,
//! quarantined operations, moved or deleted worktrees and orphaned processes.
use super::*;

#[test]
fn resume_reconciles_a_quarantined_execution_from_a_saved_handoff() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("quarantined");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let execution = crashed_execution(&mut fixture, "quarantined");
    let execution = execution["id"].as_str().unwrap();
    // The pause path clears the child identity before saving the handoff.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE executions SET child=NULL,group_id=NULL WHERE id=?1",
        [execution],
    )
    .unwrap();
    fs::write(
        path.join(".shoal.toml"),
        "[agents.claude]\nresume_command = ['sh', './resume.sh', '{prompt}']\n",
    )
    .unwrap();
    fs::write(path.join("resume.sh"), "printf restored > restored\n").unwrap();
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join(format!("{execution}.recovery.json")),
        r#"{"agent":"claude"}"#,
    )
    .unwrap();

    let output = fixture.run(&["resume", "quarantined", "--execution", execution, "--focus"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(path.join("restored")).unwrap(),
        "restored"
    );
    assert!(!state.join(format!("{execution}.recovery.json")).exists());
    assert!(
        fixture.ok(&["work", "status", "quarantined"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn daemon_restarts_keep_tracked_agents_and_commands_running() {
    let mut fixture =
        Fixture::with_config(Some("[agents.claude]\ncommand = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("restarted");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    let spawn = |args: &[&str]| {
        fixture
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut agent = spawn(&["claude", "restarted"]);
    let mut command = spawn(&[
        "run",
        "--workspace",
        "restarted",
        "--",
        "sh",
        "-c",
        "touch command-started; while [ ! -f release ]; do sleep 0.1; done; touch command-exited; exit 3",
    ]);
    wait_until("executions to start", || {
        path.join("agent-started").exists() && path.join("command-started").exists()
    });
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    let saved = || {
        let mut saved = fs::read_dir(&state)
            .map(|entries| {
                entries
                    .map(|entry| {
                        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                        name[name.find('.').unwrap()..].to_owned()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        saved.sort();
        saved
    };
    let running = |fixture: &Fixture| {
        fixture.ok(&["work", "status", "restarted"])["executions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|execution| execution["state"] == "running")
            .count()
    };
    // A restart detaches the wrappers, which keep their commands running.
    // The request is the one `shoal daemon restart` sends a managed daemon.
    let exited = fixture.raw_request(0, 1, serde_json::json!({"exit": "restart"}));
    assert_eq!(exited["type"], "ok", "{exited}");
    assert!(fixture.daemon.child.wait().unwrap().success());
    assert!(agent.try_wait().unwrap().is_none());
    assert!(command.try_wait().unwrap().is_none());
    assert!(saved().is_empty(), "{:?}", saved());
    // A command that exits while the daemon is away reports to the next one.
    fs::write(path.join("release"), "").unwrap();
    wait_until("command to exit", || path.join("command-exited").exists());
    fixture.restart();
    assert_eq!(command.wait().unwrap().code(), Some(3));
    wait_until("agent to reattach", || running(&fixture) == 1);
    // A crash leaves the agent running too.
    fixture.restart();
    assert!(agent.try_wait().unwrap().is_none());
    wait_until("agent to reattach after a crash", || running(&fixture) == 1);
    // Stopping works again and the daemon still knows the agent, which stays
    // down as a last session.
    fixture.ok(&["stop", "restarted"]);
    assert!(agent.wait().unwrap().success());
    assert_eq!(saved(), [".last-session.json"]);
    assert!(
        fixture.ok(&["work", "status", "restarted"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(fixture.ok(&["alerts", "clear"])["cleared"].as_u64() >= Some(1));
    assert_eq!(fixture.ok(&["alerts", "--all"]), Value::Array(vec![]));
}

#[test]
fn daemon_stop_and_signals_stop_tracked_agents() {
    let mut fixture =
        Fixture::with_config(Some("[agents.claude]\ncommand = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("stopped");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    let start = |fixture: &Fixture| {
        let _ = fs::remove_file(path.join("agent-started"));
        let agent = fixture
            .command()
            .args(["claude", "stopped"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_until("agent to start", || path.join("agent-started").exists());
        agent
    };
    let saved = || {
        let mut saved = fs::read_dir(&state)
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        saved.sort();
        saved
    };
    // `shoal daemon stop` stops the agent as `shoal stop` does, so it stays
    // down until `shoal resume`.
    let agent = start(&fixture);
    let output = fixture.run(&["daemon", "stop"]);
    assert!(output.status.success(), "{output:?}");
    assert!(fixture.daemon.child.wait().unwrap().success());
    let output = agent.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stopped = saved();
    assert!(
        !stopped.is_empty()
            && stopped
                .iter()
                .all(|name| name.ends_with(".last-session.json")),
        "{stopped:?}"
    );
    // A signal stops the agent saved first, as a system shutdown does.
    fixture.restart();
    let agent = start(&fixture);
    assert_eq!(
        unsafe { libc::kill(fixture.daemon.child.id() as i32, libc::SIGTERM) },
        0
    );
    assert!(fixture.daemon.child.wait().unwrap().success());
    let output = agent.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let saved = saved();
    assert!(
        saved
            .iter()
            .any(|name| !name.ends_with(".last-session.json")),
        "{saved:?}"
    );
}

pub(crate) fn repaired_workspaces(fixture: &Fixture, args: &[&str]) -> Value {
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

/// The daemon's reconciliation reports, as `work adopt` and `work setup --skip`
/// request them: `args` name the workspace, or none for all, and the options
/// as flags (`--repair`, `--stop`, `--acknowledge-stopped`, `--reclaim`).
pub(crate) fn recovery_report(fixture: &Fixture, args: &[&str]) -> Value {
    let mut workspace = None;
    let mut options = serde_json::json!({
        "repair": false, "stop": false, "acknowledge_stopped": false, "reclaim": false
    });
    for arg in args {
        match *arg {
            "--repair" => options["repair"] = true.into(),
            "--stop" => options["stop"] = true.into(),
            "--acknowledge-stopped" => options["acknowledge_stopped"] = true.into(),
            "--reclaim" => options["reclaim"] = true.into(),
            // Every workspace, as when none is named.
            "--all" => {}
            name => workspace = Some(name),
        }
    }
    let root = fixture.root.path();
    let request = serde_json::json!({
        "protocol": support::daemon_client::protocol(root),
        "id": 1,
        "method": {"reconcile": {"workspace": workspace, "options": options}},
    });
    let response = support::daemon_client::exchange(root, &request);
    assert_eq!(response["type"], "reconciled", "{response}");
    response["data"].clone()
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
    let port = fixture.ok(&["acquire", "port", "web", "--workspace", "preparing"]);
    let resource = fixture.ok(&["acquire", "resource", "lock", "--workspace", "preparing"]);
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
            let inspection = fixture.ok(&["work", "status", state]);
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
        assert_eq!(
            fixture.ok(&["leases", "port", "--workspace", "preparing"])["reserved"][0],
            port
        );
        assert_eq!(
            fixture.ok(&["leases", "resource", "--workspace", "preparing"])["leases"][0],
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
fn reconcile_repairs_interrupted_state_and_preserves_work_and_leases() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("interrupted");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("uncommitted"), "preserve me").unwrap();
    let port = fixture.ok(&["acquire", "port", "web", "--workspace", "interrupted"]);
    let resource = fixture.ok(&["acquire", "resource", "lock", "--workspace", "interrupted"]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE workspaces SET state='removing' WHERE name='interrupted'",
        [],
    )
    .unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["work", "status", "interrupted"])["workspace"]["state"],
        "failed"
    );
    let preview = recovery_report(&fixture, &["interrupted"]);
    assert_eq!(preview[0]["directory"], "valid");
    let issue = preview[0]["issues"][0].as_str().unwrap();
    assert!(issue.starts_with(preview[0]["workspace"]["error"].as_str().unwrap()));
    assert_eq!(
        fixture.ok(&["work", "status", "interrupted"])["workspace"]["state"],
        "failed"
    );
    db.execute(
        "UPDATE workspaces SET error=NULL WHERE name='interrupted'",
        [],
    )
    .unwrap();
    let preview = recovery_report(&fixture, &["interrupted"]);
    assert_eq!(preview[0]["issues"].as_array().unwrap().len(), 1);
    let repaired = repaired_workspaces(&fixture, &["interrupted", "--repair"]);
    assert_eq!(repaired[0]["workspace"]["state"], "ready");
    assert_eq!(
        fs::read_to_string(path.join("uncommitted")).unwrap(),
        "preserve me"
    );
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "interrupted"])["reserved"][0],
        port
    );
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "interrupted"])["leases"][0],
        resource
    );
    assert!(
        repaired_workspaces(&fixture, &["interrupted", "--repair"])[0]["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

pub(crate) const OWNER_MARKER: &str = "shoal-workspace";

/// Make a workspace look like one recorded before owner markers existed.
fn unmark_owner(workspace: &Value) {
    fs::remove_file(Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER)).unwrap();
}

#[test]
fn owner_marker_keeps_workspaces_verified_when_filesystem_identity_changes() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("marked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let marker = Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER);
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim_end(),
        workspace["id"].as_str().unwrap()
    );
    fs::write(path.join("dirty"), "preserve me").unwrap();
    let lease = fixture.ok(&["acquire", "resource", "lock", "--workspace", "marked"]);
    // A reboot renumbering the device, or a restore from backup, changes the
    // stat identity without changing ownership.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute("UPDATE workspaces SET git_dir_id='birth:1:2:3'", [])
        .unwrap();
    fixture.restart();
    let saved = fixture.ok(&["work", "status", "marked"])["workspace"].clone();
    assert_eq!(saved["state"], "ready", "{saved}");
    assert_eq!(saved["git_dir_id"], workspace["git_dir_id"]);
    assert!(
        fixture
            .run(&["run", "--workspace", "marked", "--", "true"])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "marked"])["leases"][0],
        lease
    );
    // A marker naming another workspace is never accepted.
    fs::write(&marker, "another-workspace\n").unwrap();
    let report = recovery_report(&fixture, &["marked", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    assert!(
        !fixture
            .run(&["run", "--workspace", "marked", "--", "true"])
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(path.join("dirty")).unwrap(),
        "preserve me"
    );
}

#[test]
fn adopting_a_workspace_path_again_reclaims_its_unverifiable_worktree() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("reclaimed");
    let other = fixture.add("other");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let path_arg = path.to_str().unwrap();
    let marker = Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER);
    fs::write(path.join("dirty"), "preserve me").unwrap();
    let lease = fixture.ok(&["acquire", "resource", "lock", "--workspace", "reclaimed"]);
    // An unmarked record whose filesystem identity changed cannot be verified.
    unmark_owner(&workspace);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE workspaces SET git_dir_id='0:0' WHERE name='reclaimed'",
        [],
    )
    .unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["work", "status", "reclaimed"])["workspace"]["state"],
        "failed"
    );
    // Reclaiming still requires the recorded branch and metadata no one else owns.
    git(path, &["switch", "-q", "-c", "elsewhere"]);
    let refused = fixture.run(&["work", "adopt", path_arg]);
    assert!(!refused.status.success());
    git(path, &["switch", "-q", "reclaimed"]);
    fs::write(&marker, format!("{}\n", other["id"].as_str().unwrap())).unwrap();
    let refused = fixture.run(&["work", "adopt", path_arg]);
    assert!(!refused.status.success());
    fs::remove_file(&marker).unwrap();
    // An explicit repository must be the recorded one.
    let refused = fixture.run(&["work", "adopt", path_arg, "--repo", "missing"]);
    assert!(!refused.status.success());
    assert_eq!(
        fixture.ok(&["work", "status", "reclaimed"])["workspace"]["state"],
        "failed"
    );

    // Reclaiming enters the worktree, as adoption does.
    let directive = fixture.root.path().join("directive");
    let output = fixture
        .command()
        .env("SHOAL_SHELL_DIRECTIVE", &directive)
        .args(["work", "adopt", path_arg])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read_to_string(&directive).unwrap().trim(), path_arg);
    let reclaimed = fixture.ok(&["work", "status", "reclaimed"])["workspace"].clone();
    assert_eq!(reclaimed["id"], workspace["id"]);
    assert_eq!(reclaimed["state"], "ready");
    assert_eq!(
        fs::read_to_string(path.join("dirty")).unwrap(),
        "preserve me"
    );
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "reclaimed"])["leases"][0],
        lease
    );
    assert!(
        fixture
            .run(&["run", "--workspace", "reclaimed", "--", "true"])
            .status
            .success()
    );
}

#[test]
fn reconcile_reclaims_unverifiable_worktrees_only_on_explicit_request() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("reclaimed");
    let other = fixture.add("other");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let marker = Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER);
    fs::write(path.join("dirty"), "preserve me").unwrap();
    let lease = fixture.ok(&["acquire", "resource", "lock", "--workspace", "reclaimed"]);
    // An unmarked record whose filesystem identity changed cannot be verified.
    unmark_owner(&workspace);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE workspaces SET git_dir_id='0:0' WHERE name='reclaimed'",
        [],
    )
    .unwrap();
    fixture.restart();
    assert_eq!(
        fixture.ok(&["work", "status", "reclaimed"])["workspace"]["state"],
        "failed"
    );
    let report = recovery_report(&fixture, &["reclaimed", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    // Reclaim still requires the recorded branch and metadata no one else owns.
    git(path, &["switch", "-q", "-c", "elsewhere"]);
    let refused = recovery_report(&fixture, &["reclaimed", "--repair", "--reclaim"]);
    assert_eq!(refused[0]["workspace"]["state"], "failed");
    git(path, &["switch", "-q", "reclaimed"]);
    fs::write(&marker, format!("{}\n", other["id"].as_str().unwrap())).unwrap();
    let refused = recovery_report(&fixture, &["reclaimed", "--repair", "--reclaim"]);
    assert_eq!(refused[0]["workspace"]["state"], "failed");
    fs::remove_file(&marker).unwrap();
    let reclaimed = repaired_workspaces(&fixture, &["reclaimed", "--repair", "--reclaim"]);
    assert_eq!(reclaimed[0]["workspace"]["state"], "ready");
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim_end(),
        workspace["id"].as_str().unwrap()
    );
    assert_eq!(
        reclaimed[0]["workspace"]["git_dir_id"],
        workspace["git_dir_id"]
    );
    assert!(
        fixture
            .run(&["run", "--workspace", "reclaimed", "--", "true"])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "reclaimed"])["leases"][0],
        lease
    );
    assert_eq!(
        fs::read_to_string(path.join("dirty")).unwrap(),
        "preserve me"
    );
    fixture.restart();
    assert_eq!(
        fixture.ok(&["work", "status", "reclaimed"])["workspace"]["state"],
        "ready"
    );
}

#[test]
fn deleted_worktrees_are_forgotten_even_when_their_metadata_is_unverifiable() {
    let fixture = Fixture::new();
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let mut paths = Vec::new();
    for name in ["deleted", "moved"] {
        let workspace = fixture.add(name);
        unmark_owner(&workspace);
        db.execute(
            "UPDATE workspaces SET git_dir_id='0:0' WHERE name=?1",
            [name],
        )
        .unwrap();
        paths.push(PathBuf::from(workspace["path"].as_str().unwrap()));
    }
    // Git still lists the deleted worktree, but nothing exists to protect.
    fs::remove_dir_all(&paths[0]).unwrap();
    let elsewhere = fixture.root.path().join("elsewhere");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            paths[1].to_str().unwrap(),
            elsewhere.to_str().unwrap(),
        ],
    );
    let report = recovery_report(&fixture, &["moved", "--repair"]);
    assert!(!report[0]["issues"].as_array().unwrap().is_empty());
    assert!(!fixture.run(&["rm", "moved", "--yes"]).status.success());
    assert!(elsewhere.join("tracked").exists());
    assert!(fixture.run(&["rm", "deleted", "--yes"]).status.success());
    let names: Vec<_> = fixture
        .ok(&["work"])
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].clone())
        .collect();
    assert_eq!(names, ["moved"]);
}

#[test]
fn legacy_worktree_identity_upgrades_only_after_verification() {
    use std::os::unix::fs::MetadataExt;

    let mut fixture = Fixture::with_config(Some("[resources.lock]\ncapacity = 3\n"));
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let mut cases = Vec::new();
    for name in ["ready", "failed", "renumbered"] {
        let workspace = fixture.add(name);
        let metadata = fs::metadata(workspace["git_dir"].as_str().unwrap()).unwrap();
        let device = metadata.dev() + u64::from(name == "renumbered");
        let legacy = format!("{device}:{}", metadata.ino());
        unmark_owner(&workspace);
        fs::write(
            Path::new(workspace["path"].as_str().unwrap()).join("dirty"),
            "preserve me",
        )
        .unwrap();
        let lease = fixture.ok(&["acquire", "resource", "lock", "--workspace", name]);
        db.execute(
            "UPDATE workspaces SET git_dir_id=?1,state=?2 WHERE id=?3",
            rusqlite::params![
                legacy,
                if name == "failed" { "failed" } else { "ready" },
                workspace["id"].as_str().unwrap()
            ],
        )
        .unwrap();
        cases.push((name, workspace, legacy, lease));
    }
    fixture.restart();
    for (name, workspace, legacy, lease) in cases {
        let saved = fixture.ok(&["work", "status", name])["workspace"].clone();
        if name == "ready" {
            assert_eq!(saved["state"], "ready");
            assert_eq!(saved["git_dir_id"], workspace["git_dir_id"]);
        } else {
            assert_eq!(saved["state"], "failed");
            assert_eq!(saved["git_dir_id"], legacy);
        }
        recovery_report(&fixture, &[name]);
        assert_eq!(fixture.ok(&["work", "status", name])["workspace"], saved);
        let report = recovery_report(&fixture, &[name, "--repair"]);
        if name == "renumbered" {
            assert_eq!(report[0]["directory"], "unverified");
            assert_eq!(report[0]["workspace"]["git_dir_id"], legacy);
            assert!(
                !fixture
                    .run(&["run", "--workspace", name, "--", "true"])
                    .status
                    .success()
            );
            assert!(!fixture.run(&["rm", name, "--yes"]).status.success());
        } else {
            assert_eq!(report[0]["workspace"]["state"], "ready");
            assert_eq!(
                report[0]["workspace"]["git_dir_id"],
                workspace["git_dir_id"]
            );
            assert!(
                fixture
                    .run(&["run", "--workspace", name, "--", "true"])
                    .status
                    .success()
            );
            let repeated = recovery_report(&fixture, &[name, "--repair"]);
            assert!(repeated[0]["changes"].as_array().unwrap().is_empty());
        }
        assert_eq!(
            fixture.ok(&["leases", "resource", "--workspace", name])["leases"][0],
            lease
        );
        assert_eq!(
            fs::read_to_string(Path::new(workspace["path"].as_str().unwrap()).join("dirty"))
                .unwrap(),
            "preserve me"
        );
    }
}

#[test]
fn legacy_identity_still_detects_missing_worktrees_that_moved() {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new();
    let workspace = fixture.add("legacy");
    let metadata = fs::metadata(workspace["git_dir"].as_str().unwrap()).unwrap();
    let legacy = format!("{}:{}", metadata.dev(), metadata.ino());
    unmark_owner(&workspace);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute("UPDATE workspaces SET git_dir_id=?1", [&legacy])
        .unwrap();
    let moved = fixture.root.path().join("moved");
    git(
        &fixture.repo,
        &[
            "worktree",
            "move",
            workspace["path"].as_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    let report = recovery_report(&fixture, &["legacy", "--repair"]);
    assert_eq!(report[0]["directory"], "moved");
    assert_eq!(report[0]["workspace"]["git_dir_id"], legacy);
    assert!(moved.join("tracked").exists());
}

#[test]
fn reconcile_detects_moved_and_replaced_worktrees_without_deleting_data() {
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
    let report = recovery_report(&fixture, &["original", "--repair"]);
    assert_eq!(report[0]["directory"], "moved");
    let preview = recovery_report(&fixture, &["original"]);
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
    repaired_workspaces(&fixture, &["original", "--repair"]);
    // Replace the admin directory at its SAME path, proving pathname checks alone
    // are insufficient. Git re-creates it without Shoal's owner marker.
    let admin = Path::new(workspace["git_dir"].as_str().unwrap());
    let old = fixture.root.path().join("old-admin");
    fs::rename(admin, &old).unwrap();
    fs::create_dir(admin).unwrap();
    for entry in fs::read_dir(&old).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() && entry.file_name() != OWNER_MARKER {
            fs::copy(entry.path(), admin.join(entry.file_name())).unwrap();
        }
    }
    let report = recovery_report(&fixture, &["original", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    assert!(!fixture.run(&["rm", "original", "--yes"]).status.success());
    assert!(
        !fixture
            .run(&["run", "--workspace", "original", "--", "true"])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(path.join("dirty")).unwrap(), "saved");
}

#[test]
fn deleted_worktrees_are_forgotten_with_their_resources_but_moved_ones_are_kept() {
    let mut fixture = Fixture::with_config(Some("[resources.lock]\ncapacity = 3\n"));
    let mut paths = Vec::new();
    // Commits of their own keep the branches from the kept-branch sweep.
    for name in ["directory-only", "git-removed", "moved"] {
        let workspace = fixture.add(name);
        commit_file(Path::new(workspace["path"].as_str().unwrap()), name, name);
        fixture.ok(&["acquire", "port", "web", "--workspace", name]);
        fixture.ok(&["acquire", "resource", "lock", "--workspace", name]);
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
    let mut last_names = Vec::new();
    try_wait_for(|| {
        let names: Vec<String> = fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap().to_owned())
            .collect();
        last_names = names.clone();
        (names == ["moved"]).then_some(())
    })
    .unwrap_or_else(|| panic!("deleted worktrees remain: {last_names:?}"));
    assert_eq!(
        fixture.ok(&["work", "status", "moved"])["workspace"]["state"],
        "failed"
    );
    assert!(!fixture.run(&["rm", "moved"]).status.success());
    assert_eq!(
        fixture
            .ok(&["leases", "port", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .ok(&["leases", "resource", "--all"])
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
    commit_file(
        Path::new(workspace["path"].as_str().unwrap()),
        "explicit",
        "work",
    );
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();
    let result = fixture.ok(&["rm", "explicit"]);
    assert_eq!(result["branch_deleted"], false);
    assert!(!git(&fixture.repo, &["rev-parse", "refs/heads/explicit"]).is_empty());

    let missing_parent = fixture.add("missing-parent");
    let missing_path = PathBuf::from(missing_parent["path"].as_str().unwrap());
    commit_file(&missing_path, "missing-parent", "work");
    fixture.ok(&["acquire", "port", "web", "--workspace", "missing-parent"]);
    fixture.ok(&[
        "acquire",
        "resource",
        "lock",
        "--workspace",
        "missing-parent",
    ]);
    git(
        &fixture.repo,
        &["worktree", "remove", missing_path.to_str().unwrap()],
    );
    fs::remove_dir_all(missing_path.parent().unwrap()).unwrap();
    let result = fixture.ok(&["rm", "missing-parent"]);
    assert_eq!(result["branch_deleted"], false);
    assert!(!git(&fixture.repo, &["rev-parse", "refs/heads/missing-parent"]).is_empty());

    assert_eq!(
        fixture
            .ok(&["leases", "port", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .ok(&["leases", "resource", "--all"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn reconcile_stops_identity_verified_orphans_after_wrapper_death() {
    let fixture = Fixture::new();
    fixture.add("orphan");
    let mut wrapper = fixture
        .command()
        .args(["run", "--workspace", "orphan", "--"])
        .args(READABLE_SLEEP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let execution = wait_registered_execution(&fixture, "orphan");
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    wait_until("orphan execution to become unknown", || {
        fixture.ok(&["work", "status", "orphan"])["executions"][0]["state"] == "unknown"
    });
    let report = recovery_report(&fixture, &["orphan", "--repair"]);
    assert!(
        report[0]["executions"][0]["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["pid"] == execution["child"]["pid"])
    );
    assert_eq!(
        fixture.ok(&["work", "status", "orphan"])["executions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    repaired_workspaces(
        &fixture,
        &["orphan", "--repair", "--stop", "--acknowledge-stopped"],
    );
    assert_eq!(
        fixture.ok(&["work", "status", "orphan"])["executions"],
        serde_json::json!([])
    );
    assert_eq!(
        fixture.ok(&["work", "status", "orphan"])["workspace"]["state"],
        "ready"
    );
}

/// Run a command in `workspace`, then crash the daemon, the wrapper and the
/// command together. That leaves nothing to reattach, only the record, which
/// the restarted daemon quarantines as unknown. Unlike a record without a
/// wrapper, its wrapper born during the test keeps the runner's older
/// processes with unreadable environments out of the evidence.
pub(crate) fn crashed_execution(fixture: &mut Fixture, workspace: &str) -> Value {
    let mut wrapper = fixture
        .command()
        .args(["run", "--workspace", workspace, "--"])
        .args(READABLE_SLEEP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let execution = wait_registered_execution(fixture, workspace);
    fixture.daemon.kill();
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    let group = execution["child"]["pid"].as_i64().unwrap() as i32;
    unsafe { libc::kill(-group, libc::SIGKILL) };
    fixture.restart();
    execution
}

#[test]
fn reconcile_recovers_daemon_crash_and_requires_acknowledgement_without_launch_identity() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("crash");
    let port = fixture.ok(&["acquire", "port", "web", "--workspace", "crash"]);
    let execution = crashed_execution(&mut fixture, "crash");
    let report = recovery_report(&fixture, &["crash"]);
    assert_eq!(report[0]["executions"][0]["state"], "unknown");
    repaired_workspaces(&fixture, &["crash", "--repair", "--acknowledge-stopped"]);
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "crash"])["reserved"][0],
        port
    );
    // A wrapper that died before launching its command recorded no child or
    // group.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO executions(id,workspace_id,state,wrapper) VALUES ('unlaunched',?1,'unknown',?2)",
        rusqlite::params![
            workspace["id"].as_str().unwrap(),
            execution["wrapper"].to_string()
        ],
    )
    .unwrap();
    let report = recovery_report(&fixture, &["crash", "--repair"]);
    assert!(!report[0]["executions"][0]["cleared"].as_bool().unwrap());
    repaired_workspaces(&fixture, &["crash", "--repair", "--acknowledge-stopped"]);
    assert_eq!(
        fixture.ok(&["work", "status", "crash"])["executions"],
        serde_json::json!([])
    );
}

#[test]
fn a_finished_command_stops_the_detached_children_it_leaves() {
    let fixture = Fixture::new();
    let workspace = fixture.add("detached");
    let script = "import pathlib, subprocess, sys; child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); pathlib.Path('child-pid').write_text(str(child.pid))";
    let output = fixture.run(&[
        "run",
        "--workspace",
        "detached",
        "--",
        "python3",
        "-c",
        script,
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        fixture.ok(&["work", "status", "detached"])["executions"],
        serde_json::json!([])
    );
    let pid = fs::read_to_string(Path::new(workspace["path"].as_str().unwrap()).join("child-pid"))
        .unwrap();
    assert!(
        !process_running(pid.parse().unwrap()),
        "the command's detached child survived its exit"
    );
}

#[test]
fn manual_removal_stops_recorded_orphans_before_releasing_resources() {
    let fixture = Fixture::with_config(Some("[resources.lock]\n"));
    fixture.add("orphan");
    fixture.ok(&["acquire", "resource", "lock", "--workspace", "orphan"]);
    let mut wrapper = fixture
        .command()
        .args(["run", "--workspace", "orphan", "--"])
        .args(READABLE_SLEEP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let execution = wait_registered_execution(&fixture, "orphan");
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    wait_until("orphan execution to become unknown", || {
        fixture.ok(&["work", "status", "orphan"])["executions"][0]["state"] == "unknown"
    });
    fixture.ok(&["rm", "orphan"]);
    assert_eq!(
        fixture.ok(&["leases", "resource", "--all"]),
        serde_json::json!([])
    );
    let pid = execution["child"]["pid"].as_u64().unwrap() as u32;
    assert!(!process_running(pid), "owned child survived removal");
}

#[test]
fn reconcile_all_reports_each_workspace_independently() {
    let fixture = Fixture::new();
    fixture.add("healthy");
    let missing = fixture.add("missing");
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    let reports = recovery_report(&fixture, &["--all", "--repair"]);
    assert_eq!(reports.as_array().unwrap().len(), 2);
    assert_eq!(reports[0]["workspace"]["name"], "healthy");
    assert_eq!(reports[0]["workspace"]["state"], "ready");
    assert_eq!(reports[1]["workspace"]["state"], "failed");
}

#[test]
fn reconcile_preserves_connected_commands_until_stop_is_explicit() {
    let fixture = Fixture::new();
    fixture.add("connected");
    let port = fixture.ok(&["acquire", "port", "web", "--workspace", "connected"]);
    let mut wrapper = fixture
        .command()
        .args(["run", "--workspace", "connected", "--"])
        .args(READABLE_SLEEP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "connected");
    let report = repaired_workspaces(&fixture, &["connected", "--repair"]);
    assert_eq!(report[0]["executions"][0]["connected"], true);
    assert_eq!(report[0]["executions"][0]["cleared"], false);
    assert!(wrapper.try_wait().unwrap().is_none());
    repaired_workspaces(&fixture, &["connected", "--repair", "--stop"]);
    wrapper.wait().unwrap();
    assert_eq!(
        fixture.ok(&["work", "status", "connected"])["executions"],
        serde_json::json!([])
    );
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "connected"])["reserved"][0],
        port
    );
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
        .args([
            "run",
            "--workspace",
            "orphan",
            "--",
            "python3",
            "-c",
            script,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "orphan");
    wait_until("orphan ready signal", || root.join("ready").exists());
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    wait_until("orphan execution to become unknown", || {
        fixture.ok(&["work", "status", "orphan"])["executions"][0]["state"] == "unknown"
    });
    let mut stopping = fixture
        .command()
        .args(["stop", "orphan"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("orphan stopping signal", || root.join("stopping").exists());
    let marker = root.join("other-ran");
    let output = fixture.run(&[
        "run",
        "--workspace",
        "other",
        "--",
        "touch",
        marker.to_str().unwrap(),
    ]);
    assert!(output.status.success());
    stopping.wait().unwrap(); // Recovery may still require acknowledgement of unreadable environments.
    assert!(
        root.join("saw-other").exists(),
        "other workspace was blocked until orphan was forcibly killed"
    );
}

#[test]
#[ignore = "manual release-mode measurement"]
#[allow(clippy::disallowed_macros)] // An ignored benchmark run with --nocapture.
fn benchmark_daemon_reads() {
    let fixture = Fixture::new();
    for name in ["bench0", "bench1", "bench2", "bench3"] {
        fixture.add(name);
    }
    let protocol = fixture.protocol();
    println!("method,clients,requests_per_s,p50_us,p95_us,p99_us");
    for (method, argument, body) in [
        ("inspect_workspace", "workspace", "inspection"),
        ("space_status", "space", "space_status"),
    ] {
        for clients in [1, 16] {
            let start = Instant::now();
            let mut latencies = thread::scope(|scope| {
                let mut tasks = Vec::new();
                for client in 0..clients {
                    let fixture = &fixture;
                    tasks.push(scope.spawn(move || {
                        let mut samples = Vec::new();
                        for _ in 0..100 {
                            let start = Instant::now();
                            let response = fixture.raw_request(
                                protocol,
                                1,
                                serde_json::json!({method: {argument: format!("bench{}", client % 4)}}),
                            );
                            assert_eq!(response["type"], body, "{response}");
                            if method == "space_status" {
                                assert!(response["data"]["diff_error"].is_null(), "{response}");
                            }
                            samples.push(start.elapsed().as_micros());
                        }
                        samples
                    }));
                }
                tasks
                    .into_iter()
                    .flat_map(|task| task.join().unwrap())
                    .collect::<Vec<_>>()
            });
            let elapsed = start.elapsed().as_secs_f64();
            latencies.sort_unstable();
            println!(
                "{method},{clients},{:.0},{},{},{}",
                latencies.len() as f64 / elapsed,
                latencies[latencies.len() / 2],
                latencies[latencies.len() * 95 / 100],
                latencies[latencies.len() * 99 / 100]
            );
        }
    }
}
