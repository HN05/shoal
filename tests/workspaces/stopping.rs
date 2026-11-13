//! Stopping and resuming agents and commands, the stop levels, and leftover
//! processes.
use super::recovery::crashed_execution;
use super::*;

/// Stopping takes chats, so completion offers them to plain `stop` as to
/// the stop levels.
#[test]
fn completion_offers_chats_to_stop() {
    let fixture = Fixture::new();
    fixture.add("planned");
    let (mut agent, id) = fixture.run_chat("planning", &[]);
    let chats = fixture.ok(&["list", "--chats"]);
    let chat = (chats.as_array().unwrap().iter())
        .find(|chat| chat["id"] == id.as_str())
        .unwrap()["number"]
        .to_string();
    let state = fixture.root.path().join("state");
    let complete = |args: &[&str]| {
        let mut words = vec!["shoal", "--state-dir", state.to_str().unwrap()];
        words.extend_from_slice(args);
        let output = fixture
            .command()
            .arg("--")
            .args(&words)
            .env("SHOAL_COMPLETE", "bash")
            .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };

    let offered = complete(&["stop", "--leftovers", ""]);
    assert!(offered.lines().any(|line| line == chat), "{offered}");
    let offered = complete(&["stop", "--yes", ""]);
    assert!(offered.lines().any(|line| line == chat), "{offered}");

    agent.kill().unwrap();
    agent.wait().unwrap();
}

/// Outside any space, the stop levels pick several spaces at once, chats
/// among them.
#[test]
fn the_stop_levels_pick_several_spaces() {
    let fixture = Fixture::new();
    let workspace = fixture.add("planned");
    let (mut agent, chat) = fixture.run_chat("planning", &[]);
    let chat = chat.as_str();
    let choice = format!("{} {chat}", workspace["id"].as_str().unwrap());

    let (output, rows) = fixture.pick(&["stop", "--leftovers"], &choice, "");

    assert!(output.status.success(), "{output:?}");
    assert!(rows.lines().any(|row| row.starts_with(chat)), "{rows}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("planned"), "{stdout}");
    assert!(stdout.contains("planning"), "{stdout}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

/// `stop --leftovers` takes only processes left behind, `stop --processes`
/// every process carrying the workspace's identity too, and plain `stop`
/// both after its agents and commands. None takes the workspace's own
/// commands that run them.
#[test]
fn stop_levels_take_leftovers_then_every_untracked_process() {
    use std::os::unix::process::ExitStatusExt as _;
    let fixture = Fixture::new();
    let workspace = fixture.add("procs");
    let path = PathBuf::from(workspace["path"].as_str().unwrap());
    let leave = |name: &str| -> u32 {
        let pid_file = fixture.root.path().join(name);
        let mut leave = Command::new("sh");
        // In the fixture's group, which a failed test stops.
        fixture.commands.join(&mut leave);
        let status = leave
            .current_dir(&path)
            .args(["-c", "sleep 300 >/dev/null 2>&1 & echo $! > \"$0\""])
            .arg(&pid_file)
            .status()
            .unwrap();
        assert!(status.success());
        fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    };
    // Started by this test, which runs, so it is no leftover. This test
    // binary keeps its environment readable, as `sleep` does not
    // on macOS.
    let mut marked = Command::new(std::env::current_exe().unwrap());
    fixture.commands.join(&mut marked);
    let mut marked = marked
        .args(["--exact", "stopping::run_until_stopped", "--ignored"])
        .env("SHOAL_WORKSPACE_ID", workspace["id"].as_str().unwrap())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

    let leftover = leave("leftover");
    // An unknown space, or a workspace in another lifecycle operation,
    // refuses the batch before any stops.
    let refused = fixture.run(&["stop", "--leftovers", &fixture.id("procs"), "missing"]);
    assert!(!refused.status.success(), "{refused:?}");
    fixture.add("busy");
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE workspaces SET state='stopping' WHERE name='busy'",
        [],
    )
    .unwrap();
    let refused = fixture.run(&[
        "stop",
        "--leftovers",
        &fixture.id("procs"),
        &fixture.id("busy"),
    ]);
    assert!(!refused.status.success(), "{refused:?}");
    db.execute("UPDATE workspaces SET state='ready' WHERE name='busy'", [])
        .unwrap();
    assert!(process_running(leftover));
    let output = fixture.root.path().join("stopped.json");
    let run = fixture.run(&[
        "run",
        "--workspace",
        &fixture.id("procs"),
        "--",
        "sh",
        "-c",
        "\"$0\" --json stop --leftovers > \"$1\"",
        support::shoal_binary(),
        output.to_str().unwrap(),
    ]);
    assert!(run.status.success(), "{run:?}");
    let stopped: Value = serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(
        stopped["stopped"],
        serde_json::json!([{"space": "procs", "processes": 1}])
    );
    assert!(!process_running(leftover));
    assert!(marked.try_wait().unwrap().is_none());

    let stopped = fixture.ok(&["stop", "--processes", &fixture.id("procs")]);
    assert_eq!(stopped["stopped"][0]["processes"], 1);
    assert!(marked.wait().unwrap().signal().is_some());

    let leftover = leave("plain");
    let stopped = fixture.ok(&["stop", &fixture.id("procs")]);
    assert_eq!(stopped["other_processes"], 1);
    assert!(!process_running(leftover));
}

/// The sweep finds a process left running from the worktree, which status
/// lists and the agent's hook names once, with the command that stops it.
#[test]
fn leftovers_reach_status_and_the_agent_hook_once() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("leaky");
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let pid_file = fixture.root.path().join("leftover");
    let mut leave = Command::new("sh");
    // In the fixture's group, which a failed test stops.
    fixture.commands.join(&mut leave);
    let status = leave
        .current_dir(&path)
        .args(["-c", "sleep 300 >/dev/null 2>&1 & echo $! > \"$0\""])
        .arg(&pid_file)
        .status()
        .unwrap();
    assert!(status.success());
    let pid: u64 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // The daemon sweeps as it starts.
    fixture.restart();
    let leftovers = wait_for("the sweep to find the leftover", || {
        let status = fixture.ok(&["status", &fixture.id("leaky")]);
        let leftovers = status["leftovers"].as_array().unwrap().clone();
        (!leftovers.is_empty()).then_some(leftovers)
    });
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    assert_eq!(leftovers[0]["pid"], pid);
    assert_eq!(leftovers[0]["command"], "sleep 300");
    assert_eq!(leftovers[0]["directory"], path.to_str().unwrap());
    assert!(leftovers[0]["started_at"].as_u64().unwrap() > 0);

    let hook = || {
        let output = fixture.run(&[
            "run",
            "--workspace",
            &fixture.id("leaky"),
            "--",
            support::shoal_binary(),
            "messages",
            "--hook",
        ]);
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let notice = hook();
    assert!(
        notice.contains(&format!("sleep 300 (PID {pid}, ")),
        "{notice}"
    );
    assert_eq!(hook(), "");
    fixture.ok(&["stop", "--leftovers", &fixture.id("leaky")]);
}

#[test]
#[ignore = "helper process tests start"]
fn run_until_stopped() {
    thread::sleep(Duration::from_secs(300));
}

#[test]
fn stop_stops_named_or_all_running_workspaces_and_marks_them_stopped() {
    let fixture = Fixture::new();
    let mut children = Vec::new();
    let running = ["first", "second", "third"];
    for name in running {
        fixture.add(name);
        children.push(
            fixture
                .command()
                .args([
                    "run",
                    "--workspace",
                    &fixture.id(name),
                    "--",
                    "sh",
                    "-c",
                    "while :; do sleep 1; done",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    fixture.add("unused");
    for name in running {
        wait_until("execution registration", || {
            !fixture.ok(&["status", &fixture.id(name)])["executions"]
                .as_array()
                .unwrap()
                .is_empty()
        });
    }
    let stopped_names = |args: &[&str]| {
        let stopped = fixture.ok(args);
        assert!(stopped["failed"].as_array().unwrap().is_empty());
        let mut names = stopped["stopped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    // One in another lifecycle operation refuses them all, leaving the others running.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let set_state = |state: &str| {
        db.execute(
            "UPDATE workspaces SET state=?1 WHERE name='second'",
            [state],
        )
        .unwrap();
    };
    set_state("preparing");
    let refused = fixture.run(&["stop", &fixture.id("first"), &fixture.id("second")]);
    assert!(!refused.status.success());
    assert_eq!(
        fixture.ok(&["status", &fixture.id("first")])["executions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    set_state("ready");
    assert_eq!(
        stopped_names(&["stop", &fixture.id("first"), &fixture.id("second")]),
        ["first", "second"]
    );
    assert_eq!(
        fixture.ok(&["status", &fixture.id("third")])["executions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(stopped_names(&["stop", "--all"]), ["third"]);
    for mut child in children {
        assert!(!child.wait().unwrap().success());
    }
    // Without agents, every workspace's commands are reported once.
    let resumed = fixture.ok(&["restore", "--all"]);
    assert_eq!(
        resumed["commands"].as_array().unwrap().len(),
        3,
        "{resumed}"
    );
    assert!(resumed["queued"].as_array().unwrap().is_empty());
    assert!(
        fixture.ok(&["restore", "--all"])["commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn resume_all_skips_the_agents_of_done_workspaces() {
    let fixture = Fixture::new();
    // The daemon starts the restore from the background.
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    for name in ["active", "finished"] {
        let workspace = fixture.add(name);
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.recovery.json")),
            r#"{"agent":"claude"}"#,
        )
        .unwrap();
    }
    fixture.ok(&["done", &fixture.id("finished")]);
    // Without a terminal the daemon starts the restore at once.
    let all = fixture.ok(&["restore", "--all", "--now"]);
    let queued = all["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1, "{all}");
    assert_eq!(queued[0]["name"], "active", "{all}");
    assert_eq!(queued[0]["state"], "starting", "{all}");
    // Forgetting still forgets the done workspace's stopped agent.
    let forgotten = fixture.ok(&["restore", "--all", "--forget"]);
    assert_eq!(
        forgotten["forgotten"],
        serde_json::json!(["active-agent", "finished-agent"]),
        "{forgotten}"
    );
}

#[test]
fn resuming_several_goes_on_after_one_fails() {
    let fixture = Fixture::new();
    for name in ["good", "broken"] {
        let workspace = fixture.add(name);
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.recovery.json")),
            r#"{"agent":"claude"}"#,
        )
        .unwrap();
        if name == "broken" {
            // Its config file cannot be read, so its agent cannot start.
            let path = Path::new(workspace["path"].as_str().unwrap());
            fs::create_dir(path.join(".shoal.toml")).unwrap();
        }
    }
    // On a terminal this process starts each restore, which reads its
    // workspace's settings.
    let (output, transcript) = fixture.terminal_command(
        fixture.command().args([
            "restore",
            &fixture.id("broken"),
            &fixture.id("good"),
            "--now",
        ]),
        "",
        true,
    );
    assert!(!output.status.success(), "{output:?}\n{transcript}");
    assert!(transcript.contains("cannot restore broken"), "{transcript}");
    assert!(!transcript.contains("cannot restore good"), "{transcript}");
}

#[test]
fn a_parent_forgets_only_the_stopped_work_of_its_children() {
    let fixture = Fixture::new();
    fixture.add("parent");
    fixture.add("other");
    let split = scoped_command(&fixture, "parent", &["split", "child"]);
    assert!(split.status.success(), "{split:?}");
    let save_agent = |name: &str| {
        let id = fixture.ok(&["status", &fixture.id(name)])["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let state = fixture.root.path().join("state/workspaces").join(id);
        fs::create_dir_all(&state).unwrap();
        let record = state.join(format!("{name}-agent.recovery.json"));
        fs::write(&record, r#"{"agent":"claude"}"#).unwrap();
        record
    };
    let records = ["parent", "other", "child"].map(save_agent);
    for args in [
        &["restore", &fixture.id("parent"), "--forget"][..],
        &["restore", &fixture.id("other"), "--forget"],
        &["restore", &fixture.id("other")],
        // A stopped agent the parent did not stop is the user's.
        &["restore", &fixture.id("child")],
        &["restore", &fixture.id("child"), "--now"],
        &["restore", "--all", "--forget"],
        &["restore"],
    ] {
        let output = scoped_command(&fixture, "parent", args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
    }
    // Clearing the token leaves the process scoped by its shell's ancestry.
    let output = fixture
        .command()
        .args([
            "run",
            "--workspace",
            &fixture.id("parent"),
            "--",
            "sh",
            "-c",
        ])
        .arg(r#"env -u SHOAL_SCOPE_TOKEN "$0" restore other --forget; exit $?"#)
        .arg(support::shoal_binary())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(records.iter().all(|record| record.exists()));
    let output = scoped_command(
        &fixture,
        "parent",
        &["restore", &fixture.id("child"), "--forget"],
    );
    assert!(output.status.success(), "{output:?}");
    let forgotten: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(forgotten["forgotten"], serde_json::json!(["child-agent"]));
    assert!(!records[2].exists());
    assert!(records[..2].iter().all(|record| record.exists()));
}

/// A parent's stop of its child stops the workspaces below it too, and the
/// parent resumes what it stopped, which the daemon starts outside the
/// parent; what the user stopped stays the user's to resume.
#[test]
fn a_parent_stops_its_children_and_resumes_only_what_it_stopped() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\n[machine.overload.disk]\nenabled = false\n[agents.waiter]\nresume_command = ['sh', '-c', 'trap \"exit 0\" TERM; touch restored; while :; do sleep 1; done']\n",
    ));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    fixture.add("parent");
    fixture.add("other");
    let split = scoped_command(&fixture, "parent", &["split", "child"]);
    assert!(split.status.success(), "{split:?}");
    let split = scoped_command(&fixture, "child", &["split", "grandchild"]);
    assert!(split.status.success(), "{split:?}");
    let paths = ["child", "grandchild"].map(|name| {
        let workspace = fixture.ok(&["status", &fixture.id(name)])["workspace"].clone();
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.last-session.json")),
            r#"{"agent":"waiter"}"#,
        )
        .unwrap();
        PathBuf::from(workspace["path"].as_str().unwrap())
    });
    let restored = || paths.iter().all(|path| path.join("restored").exists());
    fixture.ok(&["restore", &fixture.id("child"), &fixture.id("grandchild")]);
    wait_until("the user's restores", restored);
    // The user's stops are not the parent's to resume.
    for workspace in ["parent", "other"] {
        let output = scoped_command(&fixture, "parent", &["stop", &fixture.id(workspace)]);
        assert!(!output.status.success(), "{workspace}: {output:?}");
    }
    fixture.ok(&["stop", &fixture.id("child")]);
    for args in [
        &["restore", &fixture.id("child")][..],
        &["restore", &fixture.id("grandchild")],
    ] {
        let output = scoped_command(&fixture, "parent", args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
    }

    for path in &paths {
        fs::remove_file(path.join("restored")).unwrap();
    }
    fixture.ok(&["restore", &fixture.id("child"), &fixture.id("grandchild")]);
    wait_until("the user's second restores", restored);
    let output = scoped_command(&fixture, "parent", &["stop", &fixture.id("child")]);
    assert!(output.status.success(), "{output:?}");
    let stopped: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        stopped["stopped"],
        serde_json::json!(["child", "grandchild"]),
        "{stopped}"
    );
    for path in &paths {
        fs::remove_file(path.join("restored")).unwrap();
    }
    let output = scoped_command(
        &fixture,
        "parent",
        &["restore", &fixture.id("child"), &fixture.id("grandchild")],
    );
    assert!(output.status.success(), "{output:?}");
    let queued: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(queued["queued"].as_array().unwrap().len(), 2, "{queued}");
    wait_until("the parent's restores", restored);
    fixture.ok(&["stop", &fixture.id("child")]);
}

/// Stopping a workspace stops the workspaces split from it, whose agents work
/// for it, unless `--skip-children`.
#[test]
fn stopping_a_workspace_stops_the_workspaces_below_it() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\n[machine.overload.disk]\nenabled = false\n[agents.waiter]\nresume_command = ['sh', '-c', 'trap \"exit 0\" TERM; touch restored; while :; do sleep 1; done']\n",
    ));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    fixture.add("parent");
    let split = scoped_command(&fixture, "parent", &["split", "child"]);
    assert!(split.status.success(), "{split:?}");
    // Only the child has an agent, stopped so its restore queues.
    let child = fixture.ok(&["status", &fixture.id("child")])["workspace"].clone();
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(child["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("child-agent.recovery.json"),
        r#"{"agent":"waiter"}"#,
    )
    .unwrap();
    let path = PathBuf::from(child["path"].as_str().unwrap());
    fixture.ok(&["restore", &fixture.id("child")]);
    wait_until("the restore", || path.join("restored").exists());
    let running = || {
        fixture.ok(&["status", &fixture.id("child")])["executions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|execution| execution["state"] == "running")
    };

    let stopped = fixture.ok(&["stop", &fixture.id("parent"), "--skip-children"]);
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert!(running());
    let output = fixture.run(&["stop", &fixture.id("parent")]);
    assert!(output.status.success(), "{output:?}");
    assert!(!running());
}

#[test]
fn stop_saves_agents_and_commands_and_resumes_agent_sessions() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './agent.sh']\nresume_command = ['sh', './resume.sh', '{prompt}']\n",
    ));
    let workspace = fixture.add("stopped");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join("agent.sh"),
        "printf %s \"${SHOAL_OUTPUT-unset}\" > output-$SHOAL_EXECUTION_ID\ntrap 'exit 0' TERM\ntouch started-$SHOAL_EXECUTION_ID\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(
        path.join("resume.sh"),
        "printf '%s|' \"$1\" >> restored\nprintf '%s|' \"${SHOAL_OUTPUT-unset}\" >> restored-output\n",
    )
    .unwrap();
    let spawn_with = |args: &[&str], output: Option<&str>| {
        let mut command = fixture.command();
        if let Some(output) = output {
            command.env("SHOAL_OUTPUT", output);
        }
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    // Agents get JSON output unless they inherit a setting; commands get none.
    let mut children = vec![
        spawn_with(
            &[
                "start",
                "--in",
                &fixture.id("stopped"),
                "--here",
                "--now",
                "--agent",
                "claude",
            ],
            Some("text"),
        ),
        spawn_with(
            &[
                "run",
                "--workspace",
                &fixture.id("stopped"),
                "--",
                "sh",
                "-c",
                "printf %s \"${SHOAL_OUTPUT-unset}\" > command-output; touch command-started; while :; do sleep 1; done",
            ],
            None,
        ),
    ];
    let mut last_inspection = Value::Null;
    let ids = try_wait_for(|| {
        let inspection = fixture.ok(&["status", &fixture.id("stopped")]);
        last_inspection = inspection.clone();
        let ids = inspection["executions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|execution| execution["id"].as_str().unwrap().to_owned())
            .filter(|id| path.join(format!("started-{id}")).exists())
            .collect::<Vec<_>>();
        (ids.len() == 1 && path.join("command-started").exists()).then_some(ids)
    })
    .unwrap_or_else(|| panic!("executions did not start: {last_inspection}"));
    let mut outputs = ids
        .iter()
        .map(|id| fs::read_to_string(path.join(format!("output-{id}"))).unwrap())
        .collect::<Vec<_>>();
    outputs.sort();
    assert_eq!(outputs, ["text"]);
    assert_eq!(
        fs::read_to_string(path.join("command-output")).unwrap(),
        "unset"
    );
    let output = fixture
        .command()
        .current_dir(path)
        .args(["--json", "stop"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        fixture.ok(&["status", &fixture.id("stopped")])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for child in children.drain(..) {
        child.wait_with_output().unwrap();
    }
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    // The agents stay down as last sessions; the command is stopped work.
    for id in &ids {
        assert!(state.join(format!("{id}.last-session.json")).exists());
        assert!(!state.join(format!("{id}.recovery.json")).exists());
    }
    let commands = fs::read_dir(&state)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".command.json")
        })
        .count();
    assert_eq!(commands, 1);
    for id in &ids {
        assert!(
            fixture
                .run(&[
                    "restore",
                    &fixture.id("stopped"),
                    "--execution",
                    id,
                    "--focus"
                ])
                .status
                .success()
        );
        assert!(!state.join(format!("{id}.last-session.json")).exists());
    }
    // The stopped command reaches the first restored session only.
    let restored = fs::read_to_string(path.join("restored")).unwrap();
    let prompts = restored.split('|').collect::<Vec<_>>();
    assert_eq!(prompts.len(), 2, "{restored}");
    assert!(
        prompts[0].contains("- sh -c 'printf %s \"${SHOAL_OUTPUT-unset}\" > command-output; touch command-started; while :; do sleep 1; done'"),
        "{restored}"
    );
    // The restored session learns why it stopped.
    assert!(prompts[0].contains("(stopped by shoal stop)"), "{restored}");
    assert_eq!(
        fs::read_to_string(path.join("restored-output")).unwrap(),
        "json|"
    );
    // The restored agent exited on its own, leaving only its session.
    let left = fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        left.iter().all(|name| name.ends_with(".last-session.json")),
        "{left:?}"
    );
}

#[test]
fn status_shows_where_each_reported_session_keeps_its_transcript() {
    let fixture = Fixture::with_config(Some("[agents.claude]\ncommand = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("talked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    // The agent reports its session as Claude Code's hooks do, then quits.
    fs::write(
        path.join("agent.sh"),
        format!(
            "printf '{{\"session_id\":\"talked-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    let output = fixture
        .command()
        .args([
            "start",
            "--in",
            &fixture.id("talked"),
            "--here",
            "--agent",
            "claude",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    // Its files are not on this machine yet.
    let status = fixture.ok(&["status", &fixture.id("talked")]);
    assert_eq!(
        status["transcripts"],
        serde_json::json!([{
            "agent": "claude",
            "session": "talked-session",
            "execution": status["transcripts"][0]["execution"],
            "current": false,
            "files": [],
        }]),
        "{status}"
    );

    let project = fixture.root.path().join(".claude/projects/-talked");
    let transcript = project.join("talked-session.jsonl");
    let subagent = project.join("talked-session/subagents/one.jsonl");
    fs::create_dir_all(subagent.parent().unwrap()).unwrap();
    fs::write(&transcript, "{}\n").unwrap();
    fs::write(&subagent, "{}\n").unwrap();
    let status = fixture.ok(&["status", &fixture.id("talked")]);
    let found = &status["transcripts"][0];
    assert_eq!(found["path"], transcript.to_str().unwrap(), "{status}");
    assert_eq!(
        found["files"],
        serde_json::json!([transcript.to_str().unwrap(), subagent.to_str().unwrap()]),
        "{status}"
    );
}

#[test]
fn resume_continues_the_session_of_an_agent_that_exited_on_its_own_after_a_restore() {
    let fixture = Fixture::with_config(Some("[agents.claude]\ncommand = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("quit");
    let path = Path::new(workspace["path"].as_str().unwrap());
    // The agent reports its session as Claude Code's hooks do, then quits.
    fs::write(
        path.join("agent.sh"),
        format!(
            "if [ \"$1\" = --resume ]; then printf '%s' \"$2\" > resumed; exit 0; fi\n\
             printf '{{\"session_id\":\"quit-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    let output = fixture
        .command()
        .args([
            "start",
            "--in",
            &fixture.id("quit"),
            "--here",
            "--agent",
            "claude",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // The removal record keeps the last session for the restored workspace.
    fixture.ok(&["rm", "--wait", &fixture.id("quit"), "--yes"]);
    let removed = fixture.ok(&["restore"])["removed"].clone();
    let last = &removed[0]["last_sessions"][0];
    assert_eq!(
        (&last["agent"], &last["session"]),
        (
            &serde_json::json!("claude"),
            &serde_json::json!("quit-session")
        ),
        "{removed}"
    );
    let restored = fixture.ok(&["restore", &fixture.id("quit")])["reopened"][0].clone();
    assert_eq!(restored["warnings"], serde_json::json!([]));
    let output = fixture
        .command()
        .current_dir(path)
        .args(["restore", "--focus"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(path.join("resumed")).unwrap(),
        "quit-session"
    );
}

#[test]
fn stop_allows_descendants_to_finish_signal_cleanup_after_the_leader_exits() {
    let fixture = Fixture::new();
    fixture.add("worker");
    let leader = fixture.root.path().join("leader.sh");
    // Python keeps the descendant's environment readable once launchd adopts it.
    let descendant = fixture.root.path().join("descendant.py");
    let ready = fixture.root.path().join("ready");
    let cleaned = fixture.root.path().join("cleaned");
    fs::write(&leader, "trap 'exit 0' TERM\npython3 \"$1\" &\nwait\n").unwrap();
    fs::write(
        &descendant,
        r#"import os, signal, sys, time

def clean_up(*_):
    time.sleep(0.2)
    with open(os.environ['CLEANED'], 'w') as cleaned:
        cleaned.write('done\n')
    sys.exit(0)

signal.signal(signal.SIGTERM, clean_up)
open(os.environ['READY'], 'w').close()
while True:
    time.sleep(30)
"#,
    )
    .unwrap();
    let wrapper = fixture
        .command()
        .args(["run", "--workspace", &fixture.id("worker"), "--", "sh"])
        .arg(&leader)
        .arg(&descendant)
        .env("READY", &ready)
        .env("CLEANED", &cleaned)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("descendant signal handler", || ready.exists());
    fixture.ok(&["stop", &fixture.id("worker")]);
    let output = wrapper.wait_with_output().unwrap();
    assert!(
        cleaned.exists(),
        "descendant cleanup was killed: {output:?}"
    );
    assert_eq!(
        fixture.ok(&["status", &fixture.id("worker")])["executions"],
        serde_json::json!([])
    );
}

#[test]
fn stop_asks_before_forgetting_an_execution_it_cannot_prove_ended() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("legacy");
    let execution = crashed_execution(&mut fixture, "legacy");
    // A wrapper that died before launching its command recorded no group, so
    // nothing proves it ended; its birth keeps older processes out.
    let insert = |fixture: &Fixture| {
        let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
        db.execute(
            "INSERT INTO executions(id,space_id,state,wrapper) VALUES ('unlaunched',?1,'unknown',?2)",
            rusqlite::params![
                workspace["id"].as_str().unwrap(),
                execution["wrapper"].to_string()
            ],
        )
        .unwrap();
    };
    let executions = |fixture: &Fixture| {
        fixture.ok(&["status", &fixture.id("legacy")])["executions"]
            .as_array()
            .unwrap()
            .len()
    };
    // The crashed execution has a full launch record, so stop proves it ended.
    assert!(
        fixture
            .run(&["stop", &fixture.id("legacy")])
            .status
            .success()
    );
    assert_eq!(executions(&fixture), 0);
    insert(&fixture);

    let piped = fixture.run(&["stop", &fixture.id("legacy")]);
    assert!(!piped.status.success());
    assert_eq!(executions(&fixture), 1);
    assert!(fixture.ok(&["status", &fixture.id("legacy")])["workspace"]["error"].is_null());

    let (output, transcript) = fixture.interactive(&["stop", &fixture.id("legacy")], "y\n");
    assert!(output.status.success(), "{transcript}");
    assert!(transcript.contains("Treat it as stopped"), "{transcript}");
    assert_eq!(executions(&fixture), 0);

    insert(&fixture);
    assert!(
        fixture
            .run(&["stop", &fixture.id("legacy"), "--yes"])
            .status
            .success()
    );
    assert_eq!(executions(&fixture), 0);

    // Stopping several does not ask: the unproven one fails unless --yes.
    fixture.add("other");
    insert(&fixture);
    let several = fixture.run(&[
        "--json",
        "stop",
        &fixture.id("legacy"),
        &fixture.id("other"),
    ]);
    assert!(!several.status.success());
    let report: Value = serde_json::from_slice(&several.stdout).unwrap();
    assert_eq!(report["stopped"], serde_json::json!(["other"]), "{report}");
    assert_eq!(report["failed"][0]["workspace"], "legacy", "{report}");
    assert_eq!(executions(&fixture), 1);
    assert!(
        fixture
            .run(&["stop", &fixture.id("legacy"), &fixture.id("other"), "--yes"])
            .status
            .success()
    );
    assert_eq!(executions(&fixture), 0);
}
