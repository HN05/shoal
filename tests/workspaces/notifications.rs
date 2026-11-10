//! Notifications, messages, alerts, questions, attached files, and the turn
//! state and ready marks agents report.
use super::chats::start_test_chat;
use super::forge::wait_removed;
use super::*;

#[test]
fn notifications_report_conflicts_agent_exits_and_removals_once() {
    let fixture = Fixture::with_config(Some(
        "[cleanup.auto]\nenabled=false\n[resources.lock]\ncapacity=1\n",
    ));
    fixture.add("holder");
    fixture.add("waiter");
    assert_eq!(fixture.ok(&["alerts"]), serde_json::json!([]));
    fixture.ok(&["acquire", "resource", "lock", "--workspace", "holder"]);
    for _ in 0..2 {
        let busy = fixture.run(&["acquire", "resource", "lock", "--workspace", "waiter"]);
        assert_eq!(busy.status.code(), Some(2));
    }
    let taken = fixture.ok(&["acquire", "port", "web", "--workspace", "holder"])["port"]
        .as_u64()
        .unwrap();
    let moved = fixture.ok(&[
        "acquire",
        "port",
        "web",
        "--workspace",
        "waiter",
        "--port",
        &taken.to_string(),
        "--on-conflict",
        "auto",
    ]);
    assert_ne!(moved["port"], taken);
    let bin = fixture.root.path().join("bin");
    install_test_script(bin.join("claude"), "#!/bin/sh\nexit 7\n");
    let agent = fixture
        .command()
        .args(["claude", "waiter"])
        .env("PATH", fixture.restricted_path(&bin))
        .output()
        .unwrap();
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    // Plain commands are the user's own; only agent shortcuts announce their exit.
    assert!(
        fixture
            .run(&["run", "--workspace", "waiter", "--", "true"])
            .status
            .success()
    );

    let status = fixture.ok(&["daemon", "status"]);
    assert_eq!(status["daemon"]["unread_notifications"], 3);
    // The listing leaves the count to alerts and status.
    let list = fixture.run(&["list"]);
    assert!(list.status.success());
    assert!(list.stderr.is_empty(), "{list:?}");
    let scoped = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "holder",
            "--",
            support::shoal_binary(),
            "alerts",
        ])
        .output()
        .unwrap();
    assert!(!scoped.status.success());

    // A limited listing shows the oldest new entries and says how many remain.
    let first = fixture
        .command()
        .args(["--json", "alerts", "--limit", "1"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&first.stderr).starts_with("2 more"),
        "{first:?}"
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first[0]["kind"], "resource_busy", "{first}");
    let shown = fixture.ok(&["alerts"]);
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
    let kinds: Vec<_> = summary
        .iter()
        .map(|(space, kind, _)| (*space, *kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("waiter", "resource_busy"),
            ("waiter", "port_conflict"),
            ("waiter", "agent_exited")
        ],
        "{shown}"
    );
    // Each names what it is about: the holder, the port taken, the exit code.
    assert!(summary[0].2.contains("holder"), "{shown}");
    assert!(summary[1].2.contains(&taken.to_string()), "{shown}");
    assert!(summary[2].2.ends_with('7'), "{shown}");
    assert!(shown.as_array().unwrap().iter().all(|n| n["read"] == false));
    // Shown once. `--all` still has them, now read; the text form has today's
    // time first.
    assert_eq!(fixture.ok(&["alerts"]), serde_json::json!([]));
    let all = fixture.ok(&["alerts", "--all", "--limit", "2"]);
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert!(all.as_array().unwrap().iter().all(|n| n["read"] == true));
    assert_eq!(fixture.run(&["list"]).stderr, b"");
    let text = fixture.run(&["alerts", "--all"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.lines()
            .all(|line| line.contains("  waiter  ") && line.get(2..3) == Some(":")),
        "{text}"
    );
    // Once read, the same conflict is news again.
    assert_eq!(
        fixture
            .run(&["acquire", "resource", "lock", "--workspace", "waiter"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(fixture.ok(&["alerts"]).as_array().unwrap().len(), 1);

    // A follower prints the daemon's removals as they happen and marks them read.
    let mut follower = fixture
        .command()
        .args(["--json", "alerts", "--follow"])
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
    assert_eq!(fixture.acknowledge("waiter")["type"], "ok");
    wait_removed(&fixture, "waiter");
    let line = lines
        .recv_timeout(Duration::from_secs(60))
        .expect("followed notification");
    let notification: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notification["workspace"], "waiter");
    assert_eq!(notification["kind"], "workspace_removed");
    follower.kill().unwrap();
    follower.wait().unwrap();
    assert_eq!(fixture.ok(&["alerts"]), serde_json::json!([]));
    assert_eq!(
        fixture.ok(&["daemon", "status"])["daemon"]["unread_notifications"],
        0
    );
}

#[test]
fn a_chats_alert_names_it_until_withdrawn() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let in_chat = |args: &[&str]| {
        let output = fixture
            .command()
            .current_dir(&fixture.repo)
            .env("SHOAL_CHAT", &chat)
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let sent = in_chat(&["alerts", "send", "Plan ready"]);
    let id = sent["notification"].as_i64().unwrap().to_string();
    let alerts = fixture.ok(&["alerts", "--keep-unread"]);
    let alert = alerts
        .as_array()
        .unwrap()
        .iter()
        .find(|alert| alert["message"] == "Plan ready")
        .unwrap_or_else(|| panic!("{alerts}"));
    assert_eq!(alert["workspace"], "planning", "{alert}");
    in_chat(&["alerts", "withdraw", &id]);
    let alerts = fixture.ok(&["alerts"]);
    assert!(
        alerts
            .as_array()
            .unwrap()
            .iter()
            .all(|alert| alert["message"] != "Plan ready"),
        "{alerts}"
    );
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn agents_notify_the_user_about_their_own_workspace_without_completing_it() {
    let fixture = Fixture::new();
    fixture.add("sender");
    fixture.add("other");
    let output = scoped_command(
        &fixture,
        "sender",
        &["alerts", "send", "PR #12 is ready to merge"],
    );
    assert!(output.status.success(), "{output:?}");
    // A message that stopped applying is withdrawn by the number alerts send gave.
    let output = scoped_command(&fixture, "sender", &["alerts", "send", "tests are failing"]);
    assert!(output.status.success(), "{output:?}");
    let sent: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = sent["notification"].as_i64().unwrap().to_string();
    let output = scoped_command(&fixture, "sender", &["alerts", "withdraw", &id]);
    assert!(output.status.success(), "{output:?}");
    for args in [
        &["alerts", "withdraw", &id][..],
        &["alerts", "send", "hello", "--space", "other"][..],
        &["alerts", "send", "two\nlines"],
        &["alerts"],
    ] {
        let output = scoped_command(&fixture, "sender", args);
        assert!(
            !output.status.success(),
            "scoped {args:?} unexpectedly succeeded"
        );
    }
    let notifications = fixture.ok(&["alerts"]);
    let messages: Vec<_> = notifications
        .as_array()
        .unwrap()
        .iter()
        .filter(|notification| notification["kind"] == "agent_message")
        .map(|notification| (&notification["workspace"], &notification["message"]))
        .collect();
    assert_eq!(
        messages,
        [(
            &serde_json::json!("sender"),
            &serde_json::json!("PR #12 is ready to merge")
        )]
    );
    assert!(fixture.ok(&["status", "sender"])["completion"].is_null());
}

#[test]
fn notifications_kept_unread_stay_new() {
    let fixture = Fixture::new();
    fixture.add("sender");
    for message in ["first", "second"] {
        let output = scoped_command(&fixture, "sender", &["alerts", "send", message]);
        assert!(output.status.success(), "{output:?}");
    }
    let unread = || fixture.ok(&["daemon", "status"])["daemon"]["unread_notifications"].clone();
    let messages = |notifications: &[Value]| -> Vec<(i64, String)> {
        notifications
            .iter()
            .filter(|notification| notification["kind"] == "agent_message")
            .map(|notification| {
                (
                    notification["id"].as_i64().unwrap(),
                    notification["message"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let before = unread();
    let listed = fixture.ok(&["alerts", "--keep-unread"]);
    let listed = messages(listed.as_array().unwrap());
    let texts: Vec<_> = listed.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["first", "second"]);
    assert_eq!(unread(), before);
    // A limited listing counts what it printed among the ones remaining.
    let limited = fixture.run(&["alerts", "--keep-unread", "--limit", "1"]);
    let remaining = before.as_u64().unwrap() - 1;
    let note = String::from_utf8_lossy(&limited.stderr);
    assert!(note.starts_with(&format!("{remaining} ")), "{note}");

    // A follower resuming after the first sees only the second, and keeps it unread.
    let mut follower = fixture
        .command()
        .args(["--json", "alerts", "--follow", "--keep-unread"])
        .args(["--resume", &listed[0].0.to_string()])
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
    let followed = loop {
        let line = lines
            .recv_timeout(Duration::from_secs(60))
            .expect("followed notification");
        let notification: Value = serde_json::from_str(&line).unwrap();
        if let [message] = &messages(&[notification])[..] {
            break message.clone();
        }
    };
    follower.kill().unwrap();
    follower.wait().unwrap();
    assert_eq!(followed, listed[1]);
    assert_eq!(unread(), before);

    let read = fixture.ok(&["alerts"]);
    assert_eq!(messages(read.as_array().unwrap()), listed);
    assert_eq!(fixture.ok(&["alerts"]), serde_json::json!([]));
}

#[test]
fn the_wake_hook_exits_with_its_agent() {
    let fixture = Fixture::new();
    let workspace = fixture.add("waking");
    let path = Path::new(workspace["path"].as_str().unwrap());
    // The sleep stands in for the agent; the hook would otherwise wait for an
    // update that never comes.
    let mut agent = Command::new("sleep").arg("600").spawn().unwrap();
    let mut hook = fixture
        .command()
        .current_dir(path)
        .args(["internal", "wake", "--agent", &agent.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    agent.kill().unwrap();
    agent.wait().unwrap();
    // A hang guard only: the hook checks its agent every second.
    wait_until("the wake hook to exit", || {
        hook.try_wait().unwrap().is_some()
    });
    let output = hook.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn a_chats_hooks_report_its_failed_turn_and_wake_it_for_its_messages() {
    let fixture = Fixture::new();
    let (mut agent, _) = start_test_chat(&fixture, "planning");

    // A failed turn reports for the chat, which keeps the session it names.
    let mut report = fixture
        .command()
        .current_dir(&fixture.repo)
        .args([
            "--json",
            "internal",
            "turn-failed",
            "--integration",
            "claude",
        ])
        .args(["--space", "planning"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let event = r#"{"hook_event_name":"StopFailure","session_id":"failed-turn"}"#;
    let mut stdin = report.stdin.take().unwrap();
    stdin.write_all(event.as_bytes()).unwrap();
    drop(stdin);
    let output = report.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["state"], "idle", "{status}");
    let chats = fixture.request(serde_json::json!("list_chats"));
    assert_eq!(chats["data"][0]["session"], "failed-turn", "{chats}");

    // The wake hook named the chat waits for its messages.
    let mut hook = fixture
        .command()
        .current_dir(&fixture.repo)
        .args(["internal", "wake", "--agent", &agent.id().to_string()])
        .args(["--space", "planning"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let sent = fixture.request(serde_json::json!({"send_agent_message":{
        "space": "planning", "message": "Plan the next step"
    }}));
    assert_eq!(sent["type"], "ok", "{sent}");
    // A hang guard only: the message ends the wait.
    wait_until("the wake hook to exit", || {
        hook.try_wait().unwrap().is_some()
    });
    let output = hook.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let woken = String::from_utf8(output.stderr).unwrap();
    assert!(woken.contains("Plan the next step"), "{woken}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn a_title_request_reaches_only_a_running_agent() {
    let fixture = Fixture::new();
    fixture.add("titled");
    let output = fixture.run(&["rename", "--request", "titled"]);
    assert!(!output.status.success());
    assert_eq!(fixture.ok(&["messages", "titled"]), serde_json::json!([]));

    // The agent runs until the test releases it.
    let bin = fixture.root.path().join("stub-bin");
    let (started, release) = (bin.join("started"), bin.join("release"));
    install_test_script_with_mode(
        bin.join("claude"),
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.1; done\n",
            started.display(),
            release.display()
        ),
        0o700,
    );
    let mut agent = fixture
        .command()
        .args(["claude", "titled"])
        .env("PATH", fixture.restricted_path(&bin))
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    // The daemon tracks the agent before its wrapper starts it.
    wait_until("the agent to start", || started.exists());
    // An identical request the agent has not read yet collapses.
    for _ in 0..2 {
        fixture.ok(&["rename", "--request", "titled"]);
    }
    let messages = fixture.ok(&["messages", "titled"]);
    assert_eq!(messages.as_array().unwrap().len(), 1, "{messages}");
    fs::write(&release, "").unwrap();
    assert!(agent.wait().unwrap().success());
}

#[test]
fn a_message_to_all_reaches_running_chats_but_not_workspaces_without_an_agent() {
    let fixture = Fixture::new();
    fixture.add("idle");
    let (mut agent, _) = start_test_chat(&fixture, "planning");
    let repo = fixture.repo.to_str().unwrap();
    let sent = fixture.ok(&["messages", "send", "--repo", repo, "Rebase onto main"]);
    assert_eq!(
        sent,
        serde_json::json!({
            "now": {"workspaces": [], "chats": ["planning"]},
            "on_start": {"workspaces": [], "chats": []}
        })
    );
    // A --repo without a value means the current repository.
    let output = fixture
        .command()
        .args(["--json", "messages", "send", "Pull main", "--repo"])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let sent: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        sent,
        serde_json::json!({
            "now": {"workspaces": [], "chats": ["planning"]},
            "on_start": {"workspaces": [], "chats": []}
        })
    );
    // --all reaches every repository, from outside any of them.
    let sent = fixture.ok(&["messages", "send", "--all", "The network works again"]);
    assert_eq!(
        sent,
        serde_json::json!({
            "now": {"workspaces": [], "chats": ["planning"]},
            "on_start": {"workspaces": [], "chats": []}
        })
    );
    assert_eq!(fixture.ok(&["messages", "idle"]), serde_json::json!([]));
    agent.kill().unwrap();
    agent.wait().unwrap();
    for conflicting in [["--all", "--space"], ["--all", "--repo"]] {
        let output = fixture.run(&[
            "messages",
            "send",
            conflicting[0],
            conflicting[1],
            "idle",
            "hi",
        ]);
        assert!(!output.status.success(), "{output:?}");
    }
}

#[test]
fn the_user_messages_agents_who_read_their_own_messages_once() {
    let fixture = Fixture::new();
    fixture.add("receiver");
    fixture.add("other");
    fixture.ok(&[
        "messages",
        "send",
        "Stop the dev server",
        "--space",
        "receiver",
    ]);
    let output = scoped_command(
        &fixture,
        "receiver",
        &["messages", "send", "hello", "--space", "receiver"],
    );
    assert!(!output.status.success(), "agents cannot send: {output:?}");
    let output = scoped_command(&fixture, "receiver", &["messages", "other"]);
    assert!(!output.status.success(), "{output:?}");
    let output = scoped_command(&fixture, "receiver", &["messages"]);
    assert!(output.status.success(), "{output:?}");
    let messages: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(messages[0]["message"], "Stop the dev server");
    assert_eq!(messages.as_array().unwrap().len(), 1);
    assert_eq!(fixture.ok(&["messages", "receiver"]), serde_json::json!([]));

    // Agent hooks print nothing without messages, then deliver new ones as
    // context for the event they answer.
    let hook = |stdin: &str| {
        let mut child = fixture
            .command()
            .args([
                "run",
                "--workspace",
                "receiver",
                "--",
                support::shoal_binary(),
            ])
            .args(["messages", "--hook"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let event = r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#;
    assert_eq!(hook(event), "");
    fixture.ok(&["messages", "send", "Free disk space", "--space", "receiver"]);
    let output: Value = serde_json::from_str(&hook(event)).unwrap();
    let delivered = &output["hookSpecificOutput"];
    assert_eq!(delivered["hookEventName"], "PostToolUse", "{output}");
    assert!(
        delivered["additionalContext"]
            .as_str()
            .is_some_and(|context| context.contains("Free disk space")),
        "{output}"
    );
    assert_eq!(hook(event), "");
    // A message read from a file keeps its lines, indented under the first.
    let body = fixture.root.path().join("instructions.md");
    std::fs::write(&body, "Move commits:\n\tabc123 to api\n").unwrap();
    let body = body.to_str().unwrap();
    fixture.ok(&[
        "messages",
        "send",
        "--body-file",
        body,
        "--space",
        "receiver",
    ]);
    let output: Value = serde_json::from_str(&hook(event)).unwrap();
    let context = output["hookSpecificOutput"]["additionalContext"].as_str();
    assert!(
        context.is_some_and(|context| context.contains("Move commits:\n  \tabc123 to api")),
        "{output}"
    );

    // The wake hook prints the next update for the agent and exits 2, which
    // makes Claude Code start the idle agent's next turn with it. No hook
    // reports that turn starting, so the wake reports it working.
    fixture.ok(&[
        "messages",
        "send",
        "Rebase onto main",
        "--space",
        "receiver",
    ]);
    let output = fixture
        .command()
        .args(["run", "--workspace", "receiver", "--", "sh", "-c"])
        .arg(concat!(
            r#""$0" internal agent-state idle >/dev/null && "#,
            r#""$0" internal wake --agent "$1"; woke=$?; "#,
            r#""$0" status --json && exit "$woke""#
        ))
        .arg(support::shoal_binary())
        .arg(std::process::id().to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let woken = String::from_utf8(output.stderr).unwrap();
    assert!(woken.contains("Rebase onto main"), "{woken}");
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        status["workspace"]["agent_state"]["state"], "working",
        "{status}"
    );
    assert_eq!(hook(event), "");
}

#[test]
fn messages_carry_copies_of_attached_files() {
    let fixture = Fixture::new();
    fixture.add("receiver");
    fs::write(fixture.root.path().join("shot.png"), "image").unwrap();
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args([
            "messages",
            "send",
            "--attach",
            "shot.png#Login screen",
            "--space",
            "receiver",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::remove_file(fixture.root.path().join("shot.png")).unwrap();
    let messages = fixture.ok(&["messages", "receiver"]);
    assert_eq!(messages[0]["message"], "");
    let copy = Path::new(messages[0]["attachments"][0].as_str().unwrap());
    assert!(copy.starts_with(fixture.shoal_dir()), "{copy:?}");
    assert_eq!(fs::read_to_string(copy).unwrap(), "image");
    // The workspace's files list the copy after the message is read.
    let files = fixture.ok(&["files", "receiver"]);
    assert_eq!(files[0]["name"], "shot.png", "{files}");
    assert_eq!(files[0]["source"], "message", "{files}");
    assert_eq!(files[0]["title"], "Login screen", "{files}");
    assert_eq!(files[0]["path"], messages[0]["attachments"][0], "{files}");
    let paths = fixture.run(&["files", "receiver", "--path"]);
    assert_eq!(
        String::from_utf8(paths.stdout).unwrap().trim(),
        copy.display().to_string()
    );
}

#[test]
fn messages_reach_a_chat_with_their_files() {
    let fixture = Fixture::new();
    fixture.add("worker");
    let (mut agent, _) = start_test_chat(&fixture, "planning");
    // The user sends the chat a message with a file, which its inbox shows once.
    fs::write(fixture.root.path().join("shot.png"), "image").unwrap();
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["messages", "send", "Look at this", "--space", "planning"])
        .args(["--attach", "shot.png#Login screen"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let messages = fixture.ok(&["messages", "planning"]);
    assert_eq!(messages[0]["message"], "Look at this", "{messages}");
    let copy = Path::new(messages[0]["attachments"][0].as_str().unwrap());
    assert_eq!(fs::read_to_string(copy).unwrap(), "image");
    let files = fixture.ok(&["files", "planning"]);
    assert_eq!(files[0]["source"], "message", "{files}");
    assert_eq!(fixture.ok(&["messages", "planning"]), serde_json::json!([]));
    // A workspace process cannot send to a chat.
    let output = scoped_command(
        &fixture,
        "worker",
        &["messages", "send", "hi", "--space", "planning"],
    );
    assert!(!output.status.success(), "{output:?}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn files_add_attaches_copies_without_a_message() {
    let fixture = Fixture::new();
    fixture.add("receiver");
    fs::write(fixture.root.path().join("notes.md"), "plan").unwrap();
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["--json", "files", "add", "notes.md", "--space", "receiver"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let added: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(added[0]["source"], "added", "{added}");
    assert!(added[0].get("title").is_none(), "{added}");
    let copy = Path::new(added[0]["path"].as_str().unwrap());
    assert_eq!(fs::read_to_string(copy).unwrap(), "plan");
    assert_eq!(fixture.ok(&["files", "receiver"]), added);
    // A title follows the path after #, and the list shows it.
    fs::write(fixture.root.path().join("chart.html"), "<svg/>").unwrap();
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args([
            "--json",
            "files",
            "add",
            "chart.html#Latency by region",
            "--space",
            "receiver",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let titled: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(titled[0]["name"], "chart.html", "{titled}");
    assert_eq!(titled[0]["title"], "Latency by region", "{titled}");
    let deleted = fixture.ok(&["files", "rm", "chart.html", "--space", "receiver"]);
    assert_eq!(deleted, titled);
    assert_eq!(fixture.ok(&["messages", "receiver"]), serde_json::json!([]));
    // A missing file fails before anything is picked or copied.
    let missing = fixture.run(&["files", "add", "missing.md", "--space", "receiver"]);
    assert!(!missing.status.success(), "{missing:?}");

    // An idle workspace's files go without asking; every workspace's only
    // once confirmed.
    let deleted = fixture.ok(&["files", "rm", "notes.md", "--space", "receiver"]);
    assert_eq!(deleted, added);
    assert!(!copy.exists());
    let unconfirmed = fixture.run(&["files", "rm", "--all"]);
    assert!(!unconfirmed.status.success(), "{unconfirmed:?}");
    assert_eq!(
        fixture.ok(&["files", "rm", "--all", "--yes"]),
        serde_json::json!([])
    );
}

#[test]
fn a_chats_agent_attaches_files_the_user_lists_after_the_chat_ends() {
    let fixture = Fixture::new();
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    // The sleep stands in for the chat's agent.
    let mut agent = Command::new("sleep").arg("600").spawn().unwrap();
    let response = fixture.request(serde_json::json!({"start_chat":{
        "repository": repository["id"], "name": "planning", "agent": "claude",
        "pid": agent.id()
    }}));
    assert_eq!(response["type"], "chat", "{response}");
    let chat = response["data"]["id"].as_str().unwrap().to_owned();
    fs::write(fixture.repo.join("report.md"), "Plan").unwrap();
    let in_chat = |args: &[&str]| {
        let output = fixture
            .command()
            .current_dir(&fixture.repo)
            .env("SHOAL_CHAT", &chat)
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    // The chat's agent attaches, lists and deletes its chat's files.
    let added = in_chat(&["files", "add", "report.md#Plan"]);
    assert_eq!(added[0]["chat"], "planning", "{added}");
    assert!(added[0].get("workspace").is_none(), "{added}");
    assert_eq!(added[0]["title"], "Plan", "{added}");
    assert_eq!(
        added[0]["attached_by"],
        serde_json::json!({"kind": "chat", "agent": "claude", "chat": chat}),
        "{added}"
    );
    let copy = PathBuf::from(added[0]["path"].as_str().unwrap());
    assert!(copy.to_str().unwrap().contains(&chat), "{added}");
    assert_eq!(in_chat(&["files"]), added);
    fs::write(fixture.repo.join("draft.md"), "Draft").unwrap();
    in_chat(&["files", "add", "draft.md"]);
    let deleted = in_chat(&["files", "rm", "draft.md", "--yes"]);
    assert_eq!(deleted[0]["name"], "draft.md", "{deleted}");
    // The user reaches them by the chat's name, which completion offers,
    // or picks the chat among the spaces.
    assert_eq!(fixture.ok(&["files", "planning"]), added);
    let state = fixture.root.path().join("state");
    let words = [
        "shoal",
        "--state-dir",
        state.to_str().unwrap(),
        "files",
        "add",
        "report.md",
        "--space",
        "pl",
    ];
    let completed = fixture
        .command()
        .arg("--")
        .args(words)
        .env("SHOAL_COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
        .output()
        .unwrap();
    assert!(completed.status.success(), "{completed:?}");
    let completed = String::from_utf8(completed.stdout).unwrap();
    assert!(
        completed.lines().any(|line| line == "planning"),
        "{completed}"
    );
    let (output, rows) = fixture.pick(&["files", "--print"], &chat, "");
    assert!(output.status.success(), "{output:?}");
    assert!(
        rows.lines().any(|row| row.starts_with(chat.as_str())),
        "{rows}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("report.md"),
        "{output:?}"
    );

    // A stopped chat keeps its files; once it is removed, they stay listed
    // with when they go.
    agent.kill().unwrap();
    agent.wait().unwrap();
    let stopped = fixture.ok(&["files", "planning"]);
    assert!(stopped[0]["expires_at"].is_null(), "{stopped}");
    fixture.ok(&["resume", "planning", "--discard"]);
    let kept = fixture.ok(&["files", "planning"]);
    assert_eq!(kept[0]["path"], added[0]["path"], "{kept}");
    assert!(kept[0]["expires_at"].is_i64(), "{kept}");
    assert_eq!(kept[0]["chat"], "planning", "{kept}");
    assert_eq!(fs::read_to_string(&copy).unwrap(), "Plan");
    let deleted = fixture.ok(&["files", "rm", "--space", "planning"]);
    assert_eq!(deleted[0]["path"], added[0]["path"], "{deleted}");
    assert!(!copy.exists());
}

#[test]
fn done_hands_back_files_that_shoal_files_lists() {
    // Kept, so the workspace stays to show its completion.
    let fixture = Fixture::with_config(Some("[cleanup.done]\nenabled = false\n"));
    fixture.add("audit");
    fs::write(fixture.root.path().join("report.md"), "No leaks").unwrap();
    let missing = fixture.run(&["done", "audit", "--attach", "/missing/report.md"]);
    assert!(!missing.status.success(), "{missing:?}");
    assert!(fixture.ok(&["status", "audit"])["completion"].is_null());

    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args([
            "--json",
            "done",
            "audit",
            "--attach",
            "report.md#Audit findings",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let completion: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        completion["results"][0]["name"], "report.md",
        "{completion}"
    );
    let files = fixture.ok(&["files", "audit"]);
    assert_eq!(files[0]["source"], "result", "{files}");
    assert_eq!(files[0]["title"], "Audit findings", "{files}");
    let result = files[0]["path"].as_str().unwrap();
    assert_eq!(fs::read_to_string(result).unwrap(), "No leaks");
}

#[test]
fn files_rm_refuses_unknown_names_before_asking() {
    let fixture = Fixture::new();
    fixture.add("busy");
    let mut command = fixture
        .command()
        .args(["run", "--workspace", "busy", "--"])
        .args(READABLE_SLEEP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "busy");
    // Without --yes a running workspace would ask first; the name fails sooner.
    let output = fixture.run(&["--json", "files", "rm", "missing.md", "--space", "busy"]);
    command.kill().unwrap();
    command.wait().unwrap();
    assert!(!output.status.success(), "{output:?}");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(printed.contains("missing.md"), "{printed}");
}

#[test]
fn a_turns_end_shows_when_the_agents_prompt_cache_expires() {
    let fixture = Fixture::new();
    fixture.add("cached");
    // The hook reads the lifetime from the environment the agent runs with.
    let output = fixture
        .command()
        .args(["run", "--workspace", "cached", "--", "sh", "-c"])
        .arg(concat!(
            r#""$0" internal agent-state idle --ends-turn claude >/dev/null && "#,
            r#""$0" status --json"#
        ))
        .arg(support::shoal_binary())
        .env("CLAUDE_CODE_PROMPT_CACHE_TTL", "5m")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    let state = &status["workspace"]["agent_state"];
    assert_eq!(
        state["prompt_cache_expires_at"].as_i64(),
        state["since"].as_i64().map(|since| since + 300),
        "{status}"
    );
}

#[test]
fn agents_report_their_turn_state_until_their_execution_ends() {
    let fixture = Fixture::new();
    fixture.add("reporter");
    fixture.add("other");
    let output = scoped_command(
        &fixture,
        "reporter",
        &["internal", "agent-state", "waiting", "--space", "other"],
    );
    assert!(!output.status.success(), "{output:?}");
    // The execution reports, then lists itself while its state is current.
    let output = fixture
        .command()
        .args(["run", "--workspace", "reporter", "--", "sh", "-c"])
        .arg(r#""$0" internal agent-state waiting >/dev/null && "$0" --json status"#)
        .arg(support::shoal_binary())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let shown: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        shown["workspace"]["agent_state"]["state"], "waiting",
        "{shown}"
    );
    assert!(
        fixture.ok(&["status", "reporter"])["workspace"]
            .get("agent_state")
            .is_none()
    );
    let events = fixture
        .command()
        .args(["--json", "internal", "events"])
        .output()
        .unwrap();
    assert!(events.status.success());
    let states: Vec<Value> = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["kind"] == "agent_state")
        .map(|event| {
            assert_eq!(event["name"], "reporter");
            event["agent_state"]["state"].clone()
        })
        .collect();
    assert_eq!(states, [Value::from("waiting"), Value::Null]);
}

#[test]
fn agents_report_their_turn_state_from_an_unreadable_directory() {
    let fixture = Fixture::new();
    fixture.add("reporter");
    let outside = tempfile::tempdir().unwrap();
    let inside = outside.path().join("locked/inside");
    std::fs::create_dir_all(&inside).unwrap();
    // The scope names the workspace, so the hook never reads where it runs.
    let output = fixture
        .command()
        .args(["run", "--workspace", "reporter", "--", "sh", "-c"])
        .arg(concat!(
            r#"cd "$1/locked/inside" && chmod 000 "$1/locked" && "#,
            r#""$0" internal agent-state waiting >/dev/null; reported=$?; "#,
            r#"chmod 755 "$1/locked" && cd "$1" && "$0" --json status && exit $reported"#
        ))
        .arg(support::shoal_binary())
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let shown: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        shown["workspace"]["agent_state"]["state"], "waiting",
        "{shown}"
    );
}

#[test]
fn a_failed_turn_state_report_alerts_the_user() {
    let fixture = Fixture::new();
    fixture.add("reporter");
    fixture.add("other");
    for report in [
        &["internal", "agent-state", "waiting", "--space", "other"][..],
        &[
            "internal",
            "turn-failed",
            "--integration",
            "claude",
            "--space",
            "other",
        ],
    ] {
        let output = scoped_command(&fixture, "reporter", report);
        assert!(!output.status.success(), "{output:?}");
    }
    let alerts = fixture.ok(&["alerts"]);
    let failures: Vec<&Value> = alerts
        .as_array()
        .unwrap()
        .iter()
        .filter(|alert| alert["kind"] == "state_report_failed")
        .collect();
    // Both reports fail the same way, which shows once until read.
    assert_eq!(failures.len(), 1, "{alerts}");
    assert_eq!(failures[0]["workspace"], "reporter", "{alerts}");
}

#[test]
fn agent_state_reports_from_an_input_that_never_closes() {
    let fixture = Fixture::new();
    fixture.add("reporter");
    let mut child = fixture
        .command()
        .args(["internal", "agent-state", "waiting", "--space", "reporter"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Held open and silent, like a pipe or socket a script inherited.
    let stdin = child.stdin.take();
    let status = try_wait_for(|| child.try_wait().unwrap());
    if status.is_none() {
        let _ = child.kill();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(status.is_some_and(|status| status.success()), "{output:?}");
    assert_eq!(
        fixture.ok(&["status", "reporter"])["workspace"]["agent_state"]["state"],
        "waiting"
    );
}

#[test]
fn agents_mark_their_workspace_ready_for_review_until_new_commits() {
    let fixture = Fixture::new();
    let added = fixture.add("reviewed");
    fixture.add("other");
    let path = Path::new(added["path"].as_str().unwrap());
    let output = scoped_command(&fixture, "reviewed", &["ready"]);
    assert!(output.status.success(), "{output:?}");
    let marks: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(marks[0]["url"].is_null() && marks[0]["stale"] == false);
    for args in [
        &["ready", "--space", "other"][..],
        &["ready", "pr"],
        &["ready", "--undo", "pr", "12"],
    ] {
        let output = scoped_command(&fixture, "reviewed", args);
        assert!(
            !output.status.success(),
            "scoped {args:?} unexpectedly succeeded"
        );
    }
    let listed = fixture.ok(&["inbox"]);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["kind"], "ready");
    assert_eq!(listed[0]["workspace"]["name"], "reviewed");

    fs::write(path.join("later"), "later\n").unwrap();
    git(path, &["add", "later"]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "later",
        ],
    );
    // An outdated mark no longer needs the user.
    assert_eq!(fixture.ok(&["inbox"]), serde_json::json!([]));
    assert_eq!(
        fixture.ok(&["status", "reviewed"])["workspace"]["review"][0]["stale"],
        true
    );

    let output = scoped_command(&fixture, "reviewed", &["ready", "--undo"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        fixture.ok(&["status", "reviewed"])["workspace"]
            .get("review")
            .is_none()
    );
    let output = scoped_command(&fixture, "reviewed", &["ready", "--undo"]);
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn agents_withdraw_questions_they_no_longer_need_answered() {
    let fixture = Fixture::new();
    fixture.add("asker");
    let output = scoped_command(
        &fixture,
        "asker",
        &[
            "questions",
            "ask",
            "Which database?",
            "--recommend",
            "SQLite",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let question: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = question["number"].as_str().unwrap().to_owned();
    let output = scoped_command(&fixture, "asker", &["questions", "withdraw", &id]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fixture.ok(&["questions"]),
        serde_json::json!({"questions": [], "omitted": 0})
    );
    let refused = fixture.run(&["questions", &id, "--answer", "Postgres"]);
    assert!(!refused.status.success(), "{refused:?}");
}

#[test]
fn a_chats_agent_asks_the_user_and_reads_the_answer_in_its_inbox() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let in_chat = |args: &[&str]| {
        let output = fixture
            .command()
            .current_dir(&fixture.repo)
            .env("SHOAL_CHAT", &chat)
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let asked = in_chat(&[
        "questions",
        "ask",
        "Which database?",
        "--recommend",
        "SQLite",
    ]);
    assert_eq!(asked["space"], "planning", "{asked}");
    let id = asked["number"].as_str().unwrap().to_owned();
    // The user sees it under the chat's name, and the answer reaches the chat.
    let listed = fixture.ok(&["questions", "--space", "planning"]);
    assert_eq!(
        listed["questions"][0]["number"], asked["number"],
        "{listed}"
    );
    let inbox = fixture.ok(&["inbox"]);
    let item = &inbox[0]["question"];
    assert_eq!(item["space"], "planning", "{inbox}");
    fixture.ok(&["questions", &id, "--answer", "Postgres"]);
    let messages = in_chat(&["messages"]);
    assert!(
        messages[0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Postgres")),
        "{messages}"
    );
    // The chat withdraws a question it no longer needs answered.
    let asked = in_chat(&["questions", "ask", "Which cache?", "--recommend", "None"]);
    let id = asked["number"].as_str().unwrap().to_owned();
    in_chat(&["questions", "withdraw", &id]);
    assert_eq!(
        fixture.ok(&["questions"]),
        serde_json::json!({"questions": [], "omitted": 0})
    );
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn agents_wait_for_the_answer_to_their_question() {
    let fixture = Fixture::new();
    fixture.add("asker");
    // Unscoped, so the workspace can be renamed while it waits.
    let asker = fixture
        .command()
        .args([
            "--json",
            "questions",
            "ask",
            "Which database?",
            "--recommend",
            "SQLite",
        ])
        .args(["--choice", "SQLite", "--choice", "Postgres"])
        .args(["--space", "asker", "--wait"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let question = wait_for("the question", || {
        fixture.ok(&["questions"])["questions"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
    });
    assert_eq!(question["space"], "asker");
    assert_eq!(question["recommendation"], "SQLite");
    let id = question["number"].as_str().unwrap().to_owned();

    let output = scoped_command(&fixture, "asker", &["questions", &id, "--answer", "1"]);
    assert!(!output.status.success(), "agents cannot answer: {output:?}");
    let renamed = fixture.rename("asker", "renamed");
    assert_eq!(renamed["data"]["name"], "renamed", "{renamed}");
    fixture.ok(&["questions", &id, "--answer", "2"]);
    let output = asker.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let answered: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answered["answer"], "Postgres");
    // The wait printed the answer, so its message is not delivered again.
    assert_eq!(fixture.ok(&["messages", "renamed"]), serde_json::json!([]));
    assert_eq!(
        fixture.ok(&["questions"]),
        serde_json::json!({"questions": [], "omitted": 0})
    );

    let output = scoped_command(
        &fixture,
        "renamed",
        &["questions", "ask", "Which cache?", "--recommend", "None"],
    );
    assert!(output.status.success(), "{output:?}");
    let open: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(open["answer"], Value::Null);
    // The delivered answer freed the first question's number.
    assert_eq!(open["number"], question["number"]);
    assert_eq!(
        fixture.ok(&["questions"])["questions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // An upgraded release resumes a wait by the recorded number and the
    // question's text, without asking again.
    let space = open["space_id"].as_str().unwrap();
    let id = open["number"].as_str().unwrap().to_owned();
    let item = open["item"].to_string();
    let resumed = fixture
        .command()
        .args([
            "--json",
            "questions",
            "ask",
            "Which cache?",
            "--recommend",
            "None",
            "--wait",
        ])
        .args(["--space", space, "--resume", &item])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    fixture.ok(&["questions", &id, "--answer", "Redis"]);
    let output = resumed.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let answered: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answered["answer"], "Redis");
    assert_eq!(
        fixture.ok(&["questions"]),
        serde_json::json!({"questions": [], "omitted": 0})
    );
}
