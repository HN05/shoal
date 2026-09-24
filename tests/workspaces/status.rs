use super::permits::RESOURCE_CONFIG;
use crate::support::{Fixture, git};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

#[test]
fn status_summarizes_current_workspace_work_and_supports_json() {
    let config = format!("{RESOURCE_CONFIG}\n[pr_cleanup]\nenabled=false\n");
    let fixture = Fixture::with_config(Some(&config));
    let workspace = fixture.add("summary");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "changed\nagain\n").unwrap();
    fs::write(path.join("added"), "new\n").unwrap();
    git(path, &["add", "added"]);
    fixture.ok(&["port", "acquire", "web", "summary"]);
    fixture.ok(&[
        "resource", "acquire", "devices", "summary", "--name", "tests",
    ]);
    fixture.ok(&["resource", "acquire", "signing", "summary"]);
    fixture.add("waiter");
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "signing", "waiter"])
            .status
            .code(),
        Some(2)
    );
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
        rusqlite::params![
            workspace["id"].as_str().unwrap(),
            serde_json::json!({
                "url": "https://forge.example/team/repo/pulls/7",
                "head": null,
                "error": null
            })
            .to_string()
        ],
    )
    .unwrap();

    let started = fixture.root.path().join("status-started");
    let finish = fixture.root.path().join("status-finish");
    let mut execution = fixture
        .command()
        .args([
            "exec",
            "summary",
            "--",
            "sh",
            "-c",
            "touch \"$1\"; while test ! -f \"$2\"; do sleep 0.02; done",
            "status-test",
            started.to_str().unwrap(),
            finish.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.exists() {
        assert!(Instant::now() < deadline, "execution did not start");
        thread::sleep(Duration::from_millis(20));
    }

    let output = fixture
        .command()
        .args(["--json", "status"])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["workspace"]["name"], "summary");
    assert_eq!(status["workspace"]["branch"], "summary");
    assert_eq!(status["workspace"]["state"], "ready");
    assert_eq!(status["setup_finished"], true);
    assert_eq!(
        status["diff"],
        serde_json::json!({"files_changed": 2, "insertions": 3, "deletions": 1})
    );
    assert_eq!(status["executions"].as_array().unwrap().len(), 1);
    assert_eq!(status["ports"].as_array().unwrap().len(), 1);
    assert_eq!(status["resources"].as_array().unwrap().len(), 2);
    assert_eq!(status["simulators"], serde_json::json!([]));
    assert_eq!(
        status["pr_cleanup"]["url"],
        "https://forge.example/team/repo/pulls/7"
    );
    assert_eq!(status["unread_notifications"], 1);

    let text = fixture.run(&["status", "summary"]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    for expected in [
        "summary  ready",
        "Branch:        summary",
        "Setup:         finished",
        "Changes:       2 files, +3 -1",
        "Executions:    1",
        "Ports:         1",
        "Simulators:    0",
        "Resources:     2",
        "PR watch:      https://forge.example/team/repo/pulls/7",
        "Notifications: 1 unread",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text:?}");
    }
    let missing = fixture.run(&["status"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("missing argument; pass an explicit target/name")
    );

    fs::write(finish, "done").unwrap();
    assert!(execution.wait().unwrap().success());
}

#[test]
fn status_keeps_shared_state_when_the_worktree_is_missing() {
    let fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n"));
    let workspace = fixture.add("missing-status");
    fixture.ok(&["port", "acquire", "web", "missing-status"]);
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();

    let status = fixture.ok(&["status", "missing-status"]);
    assert_eq!(status["workspace"]["name"], "missing-status");
    assert_eq!(status["ports"].as_array().unwrap().len(), 1);
    assert!(status["diff"].is_null());
    assert!(!status["diff_error"].as_str().unwrap().is_empty());

    let text = fixture.run(&["status", "missing-status"]);
    assert!(text.status.success());
    assert!(
        String::from_utf8(text.stdout)
            .unwrap()
            .contains("Changes:       unavailable")
    );
}
