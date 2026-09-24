use crate::support::{Fixture, wait_removed};
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio, thread, time::Duration};

#[test]
fn notifications_report_conflicts_agent_exits_and_removals_once() {
    let fixture = Fixture::with_config(Some(
        "[auto_cleanup]\nenabled=false\n[resources.lock]\ncapacity=1\n",
    ));
    fixture.add("holder");
    fixture.add("waiter");
    assert_eq!(
        fixture.run(&["notifications"]).stdout,
        b"No new notifications\n"
    );
    fixture.ok(&["resource", "acquire", "lock", "holder"]);
    for _ in 0..2 {
        let busy = fixture.run(&["resource", "acquire", "lock", "waiter"]);
        assert_eq!(busy.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&busy.stdout).contains("held by holder"),
            "{busy:?}"
        );
    }
    let taken = fixture.ok(&["port", "acquire", "web", "holder"])["port"]
        .as_u64()
        .unwrap();
    let moved = fixture.ok(&[
        "port",
        "acquire",
        "web",
        "waiter",
        "--port",
        &taken.to_string(),
        "--on-conflict",
        "auto",
    ]);
    assert_ne!(moved["port"], taken);
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("claude"), "#!/bin/sh\nexit 7\n").unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let agent = fixture
        .command()
        .args(["claude", "waiter"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    // Plain commands are the user's own; only agent shortcuts announce their exit.
    assert!(
        fixture
            .run(&["exec", "waiter", "--", "true"])
            .status
            .success()
    );

    let status = fixture.ok(&["daemon", "status"]);
    assert_eq!(status["daemon"]["unread_notifications"], 3);
    let list = fixture.run(&["list"]);
    assert!(list.status.success());
    assert_eq!(
        String::from_utf8_lossy(&list.stderr),
        "3 new notifications; run shoal notifications\n"
    );
    let scoped = fixture
        .command()
        .args([
            "exec",
            "holder",
            "--",
            env!("CARGO_BIN_EXE_shoal"),
            "notifications",
        ])
        .output()
        .unwrap();
    assert!(!scoped.status.success());

    // A limited listing shows the oldest new entries and says how many remain.
    let first = fixture
        .command()
        .args(["--json", "notifications", "--limit", "1"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&first.stderr),
        "2 more new notifications; run shoal notifications again\n"
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first[0]["kind"], "resource_busy", "{first}");
    let shown = fixture.ok(&["notifications"]);
    assert_eq!(shown.as_array().unwrap().len(), 2, "{shown}");
    let mut shown = shown.as_array().unwrap().clone();
    shown.insert(0, first[0].clone());
    let shown = Value::Array(shown);
    let summary: Vec<(&str, &str, &str)> = shown
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["workspace"].as_str().unwrap(),
                n["kind"].as_str().unwrap(),
                n["message"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(summary.len(), 3, "{shown}");
    assert_eq!(
        summary[0],
        (
            "waiter",
            "resource_busy",
            "no compatible capacity for any resource in pool lock; held by holder"
        )
    );
    assert_eq!(
        (summary[1].0, summary[1].1),
        ("waiter", "port_conflict"),
        "{shown}"
    );
    assert!(
        summary[1]
            .2
            .starts_with(&format!("port web: {taken} is in use; reserved ")),
        "{shown}"
    );
    assert_eq!(
        summary[2],
        ("waiter", "agent_exited", "claude exited with code 7")
    );
    assert!(shown.as_array().unwrap().iter().all(|n| n["read"] == false));
    // Shown once. `--all` still has them, now read; the text form has the time first.
    assert_eq!(fixture.ok(&["notifications"]), serde_json::json!([]));
    let all = fixture.ok(&["notifications", "--all", "--limit", "2"]);
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert!(all.as_array().unwrap().iter().all(|n| n["read"] == true));
    assert_eq!(fixture.run(&["list"]).stderr, b"");
    let text = fixture.run(&["notifications", "--all"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.lines()
            .all(|line| line.contains("  waiter  ") && line.as_bytes()[4] == b'-'),
        "{text}"
    );
    // Once read, the same conflict is news again.
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "lock", "waiter"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(fixture.ok(&["notifications"]).as_array().unwrap().len(), 1);

    // A follower prints the daemon's removals as they happen and marks them read.
    let mut follower = fixture
        .command()
        .args(["--json", "notifications", "--follow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = follower.stdout.take().unwrap();
    let (sender, lines) = std::sync::mpsc::channel();
    thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    fixture.ok(&["pr", "merged", "waiter"]);
    wait_removed(&fixture, "waiter");
    let line = lines
        .recv_timeout(Duration::from_secs(15))
        .expect("followed notification");
    let notification: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notification["workspace"], "waiter");
    assert_eq!(notification["kind"], "workspace_removed");
    assert_eq!(
        notification["message"],
        "removed after the merge acknowledgement"
    );
    follower.kill().unwrap();
    follower.wait().unwrap();
    assert_eq!(fixture.ok(&["notifications"]), serde_json::json!([]));
    assert_eq!(
        fixture.ok(&["daemon", "status"])["daemon"]["unread_notifications"],
        0
    );
}
