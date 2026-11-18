//! Chats: their sessions and places, renames, the spaces below them and
//! their agents' scope.
use super::forge::{forge_origin, github_issue, install_api_curl, script_api};
use super::herdr::{
    herdr_call, herdr_calls, herdr_worker, herdr_worker_command, install_fake_herdr,
};
use super::start::guidance_end;
use super::zmx::{wait_zmx_sessions_ended, zmx_calls};
use super::*;

/// A fixture whose Claude chat agent reports its session as Claude Code's
/// hook does, writes to its temporary directory, then talks until a signal
/// ends it; resumed, it only records how it ran.
pub(crate) fn chat_session_fixture() -> Fixture {
    chat_session_fixture_with("")
}

/// A [`chat_session_fixture`] with the repository `config` added.
fn chat_session_fixture_with(config: &str) -> Fixture {
    let fixture = Fixture::with_config(Some(&format!(
        "[agents.claude]\ncommand = ['sh', './chat-agent.sh']\n{config}"
    )));
    fs::write(
        fixture.repo.join("chat-agent.sh"),
        format!(
            "printf '%s\\n' \"$@\" > \"$HOME/chat-args\"\n\
             echo \"$SHOAL_SPACE\" > \"$HOME/chat-id\"\n\
             echo \"$TMPDIR\" > \"$HOME/chat-tmpdir\"\n\
             printf '{{\"session_id\":\"chat-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n\
             case \" $* \" in *' --resume '*) exit 0;; esac\n\
             trap 'exit 0' TERM\n\
             touch \"$TMPDIR/notes\" \"$HOME/chat-started\"\n\
             while :; do sleep 1; done\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    fixture
}

/// Run the chat `talk` of a [`chat_session_fixture`] here until `stop`,
/// given the fixture, ends the daemon, which saves and stops it. Returns the
/// chat as listed while it ran.
pub(crate) fn save_running_chat(fixture: &Fixture, stop: impl FnOnce(&Fixture)) -> Value {
    let home = fixture.root.path();
    let chat = std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            fixture.interactive_command(
                fixture.command().current_dir(&fixture.repo).args([
                    "start", "--mode", "chat", "--here", "--agent", "claude", "--name", "talk",
                ]),
                "",
            )
        });
        wait_until("chat started", || home.join("chat-started").exists());
        let chats = fixture.ok(&["list", "--chats"]);
        stop(fixture);
        let (output, transcript) = chat.join().unwrap();
        assert!(
            output.status.success(),
            "{}\n{transcript}",
            printed(&output)
        );
        chats[0].clone()
    });
    assert_eq!(chat["name"], "talk", "{chat}");
    chat
}

#[test]
fn a_saved_chats_open_question_stays_in_the_inbox() {
    let mut fixture = chat_session_fixture();
    save_running_chat(&fixture, |fixture| {
        let chat = fixture.ok(&["list", "--chats"])[0]["id"].clone();
        let output = fixture
            .command()
            .current_dir(&fixture.repo)
            .env("SHOAL_SPACE", chat.as_str().unwrap())
            .args(["--json", "questions", "ask", "Which database?"])
            .args(["--recommend", "SQLite"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", printed(&output));
        let output = fixture.run(&["daemon", "stop"]);
        assert!(output.status.success(), "{}", printed(&output));
    });
    assert!(fixture.daemon.child.wait().unwrap().success());
    fixture.restart();
    let inbox = fixture.ok(&["inbox"]);
    assert_eq!(inbox[0]["question"]["space"], "talk", "{inbox}");
    let listed = fixture.ok(&["questions", "--space", &fixture.id("talk")]);
    assert_eq!(listed["questions"][0]["space"], "talk", "{listed}");
}

#[test]
fn a_chat_daemon_stop_saved_resumes_under_its_name_and_session() {
    let mut fixture = chat_session_fixture();
    let home = fixture.root.path().to_owned();
    // `shoal daemon stop` saves the chat for `shoal restore`.
    let chat = save_running_chat(&fixture, |fixture| {
        let output = fixture.run(&["daemon", "stop"]);
        assert!(output.status.success(), "{}", printed(&output));
    });
    assert!(fixture.daemon.child.wait().unwrap().success());
    fixture.restart();
    let id = chat["id"].as_str().unwrap();
    let tmp_dir = PathBuf::from(fs::read_to_string(home.join("chat-tmpdir")).unwrap().trim());

    // A workspace process neither restores nor forgets it, even with its
    // scope token cleared.
    fixture.add("worker");
    let output = fixture
        .command()
        .args([
            "run",
            "--workspace",
            &fixture.id("worker"),
            "--",
            "sh",
            "-c",
        ])
        .arg(r#"env -u SHOAL_SCOPE_TOKEN "$0" restore talk --forget; exit $?"#)
        .arg(support::shoal_binary())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{}", printed(&output));
    let saved = fixture.ok(&["list", "--chats"]);
    assert_eq!(saved[0]["id"], id, "{saved}");
    assert_eq!(saved[0]["name"], "talk", "{saved}");
    assert_eq!(saved[0]["running"], false, "{saved}");
    assert!(tmp_dir.join("notes").exists());

    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .args(["restore", &fixture.id("talk"), "--focus"]),
        "",
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let args = fs::read_to_string(home.join("chat-args")).unwrap();
    assert!(args.contains("--resume\nchat-session\n"), "{args}");
    // Nothing waits for it, so it starts no turn of its own.
    assert!(!args.contains("Shoal: new messages"), "{args}");
    assert_eq!(fs::read_to_string(home.join("chat-id")).unwrap().trim(), id);
    assert_eq!(
        fs::read_to_string(home.join("chat-tmpdir")).unwrap().trim(),
        tmp_dir.to_str().unwrap()
    );
    // The resumed agent exited, which stops the chat again.
    assert_eq!(fixture.ok(&["list", "--chats"])[0]["running"], false);
    assert!(tmp_dir.exists());
    assert_eq!(chat_kinds(&fixture), ["created"]);
}

#[test]
fn a_resume_takes_saved_chats_and_workspaces_together() {
    let mut fixture = chat_session_fixture();
    let home = fixture.root.path().to_owned();
    let save_chat = |fixture: &mut Fixture| {
        let _ = fs::remove_file(home.join("chat-started"));
        let chat = save_running_chat(fixture, |fixture| {
            let output = fixture.run(&["daemon", "stop"]);
            assert!(output.status.success(), "{}", printed(&output));
        });
        assert!(fixture.daemon.child.wait().unwrap().success());
        fixture.restart();
        chat["id"].as_str().unwrap().to_owned()
    };
    let stop_agent = |fixture: &Fixture, workspace: &Value| {
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("worker-agent.recovery.json"),
            r#"{"agent":"claude","mode":"worker"}"#,
        )
        .unwrap();
    };
    // The chat stays, its stopped agent forgotten, so the next round
    // removes it before it starts another.
    let forgotten_both = |fixture: &Fixture, chat: &str, forgotten: &Value| {
        let forgotten = forgotten["forgotten"].as_array().unwrap();
        assert_eq!(forgotten.len(), 2, "{forgotten:?}");
        assert!(forgotten.contains(&"worker-agent".into()), "{forgotten:?}");
        let chats = fixture.ok(&["list", "--chats"]);
        assert_eq!(chats[0]["running"], false, "{chats}");
        fixture.ok(&["rm", "--wait", chat, "--yes"]);
    };
    let chat = save_chat(&mut fixture);
    let worker = fixture.add("worker");
    stop_agent(&fixture, &worker);
    // A list naming a chat and a workspace acts on both.
    let forgotten = fixture.ok(&["restore", &chat, &fixture.id("worker"), "--forget"]);
    forgotten_both(&fixture, &chat, &forgotten);
    // `--all` takes the stopped chats too.
    let chat = save_chat(&mut fixture);
    stop_agent(&fixture, &worker);
    forgotten_both(
        &fixture,
        &chat,
        &fixture.ok(&["restore", "--all", "--forget"]),
    );
    // The picker offers the stopped chats after the workspaces with stopped
    // work, then the removed chats.
    let chat = save_chat(&mut fixture);
    stop_agent(&fixture, &worker);
    let both = format!("{chat} {}", worker["id"].as_str().unwrap());
    let (output, rows) = fixture.pick(&["restore", "--forget"], &both, "");
    assert!(output.status.success(), "{}", printed(&output));
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert!(rows[0].contains("worker"), "{rows:?}");
    assert!(rows[1].contains("talk"), "{rows:?}");
    assert!(
        rows[2..].iter().all(|row| row.contains("removed")),
        "{rows:?}"
    );
}

/// `shoal daemon stop` names the spaces whose work it stopped, here a done
/// chat, and no other; `shoal restore --all` leaves out that chat's
/// stopped agent, which only naming the chat restores, and restores the
/// stopped agents beside it.
#[test]
fn restore_all_skips_a_done_chat_beside_a_stopped_workspace() {
    let mut fixture = chat_session_fixture_with("[cleanup.done]\nenabled = false\n");
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let worker = fixture.add("worker");
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(worker["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("worker-agent.recovery.json"),
        r#"{"agent":"claude","mode":"worker"}"#,
    )
    .unwrap();
    let mut stopped = Value::Null;
    let chat = save_running_chat(&fixture, |fixture| {
        let chat = fixture.ok(&["list", "--chats"])[0]["id"].clone();
        fixture.ok(&["done", chat.as_str().unwrap()]);
        stopped = fixture.ok(&["daemon", "stop"]);
    });
    assert!(fixture.daemon.child.wait().unwrap().success());
    assert_eq!(
        stopped["stopped_spaces"],
        serde_json::json!([chat["number"]]),
        "{stopped}"
    );
    fixture.restart();

    let restored = fixture.ok(&["restore", "--all"]);
    let names: Vec<_> = (restored["queued"].as_array().unwrap().iter())
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["worker"], "{restored}");
}

#[test]
fn a_chat_a_signal_saved_between_turns_waits_then_comes_back_in_its_place() {
    let mut fixture = chat_session_fixture();
    let home = fixture.root.path().to_owned();
    // A signal, as a shutdown or a logout sends, saves the chat to come back.
    let chat = save_running_chat(&fixture, |fixture| {
        assert_eq!(
            unsafe { libc::kill(fixture.daemon.child.id() as i32, libc::SIGTERM) },
            0
        );
    });
    assert!(fixture.daemon.child.wait().unwrap().success());
    fs::remove_file(home.join("chat-args")).unwrap();
    // The next daemon opens the chat in a zmx session, which runs it on a
    // terminal of its own, as a chat needs.
    install_test_script(home.join("bin").join("zmx"), terminal_zmx());
    fixture.restart();

    // Its agent stopped between turns, so its restore waits for an update,
    // as a workspace agent's does; starting the entry wakes it at once.
    let id = chat["id"].as_str().unwrap();
    wait_until("restore waiting for an update", || {
        fixture
            .ok(&["queue"])
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["space_id"] == id && entry["waits_for_update"] == true)
    });
    fixture.ok(&["queue", "start", id]);

    // The resumed agent records its arguments, then its chat, then exits.
    wait_until("chat restored", || {
        fs::read_to_string(home.join("chat-args"))
            .is_ok_and(|args| args.contains("--resume\nchat-session\n"))
    });
    // The fake opened it, not a zmx the runner may have installed.
    let calls = fs::read_to_string(home.join("zmx-calls")).unwrap();
    assert!(calls.contains("\"run\""), "{calls}");
    wait_until("chat-id rewritten", || {
        fs::read_to_string(home.join("chat-id")).is_ok_and(|written| written.trim() == id)
    });
    wait_until("restored chat stopped", || {
        fixture.ok(&["list", "--chats"])[0]["running"] == false
    });
    assert_eq!(chat_kinds(&fixture), ["created"]);
}

/// The kinds of the journal's chat creations, renames and removals, in
/// order.
fn chat_kinds(fixture: &Fixture) -> Vec<String> {
    journal(fixture)
        .into_iter()
        .filter(|event| event["space_kind"] == "chat")
        .map(|event| event["kind"].as_str().unwrap().to_owned())
        .filter(|kind| ["created", "renamed", "removed"].contains(&kind.as_str()))
        .collect()
}

/// The journal's first record of a chat's creation.
fn chat_created(fixture: &Fixture) -> Value {
    journal(fixture)
        .into_iter()
        .find(|event| event["kind"] == "created" && event["space_kind"] == "chat")
        .unwrap()
}

/// A chat's agent that changes into a workspace below the chat acts on that
/// workspace, as a workspace's agent acts on its child, and on the chat
/// anywhere else.
#[test]
fn a_chats_commands_act_on_the_workspace_below_it_whose_directory_they_run_in() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let output = fixture
        .command()
        .args([
            "--json",
            "start",
            "--wait",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "opened",
        ])
        .env("SHOAL_SPACE", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    let opened: Value = serde_json::from_slice(&output.stdout).unwrap();
    // A workspace that a chat below the chat opened is below it too.
    let (mut nested_agent, nested) = fixture.run_chat("nested", &["--parent", &chat]);
    let output = fixture
        .command()
        .args([
            "--json",
            "start",
            "--wait",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "deeper",
        ])
        .env("SHOAL_SPACE", &nested)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    let deeper: Value = serde_json::from_slice(&output.stdout).unwrap();
    let other = fixture.add("other");
    let rename_in = |directory: &Value, name: &str| {
        let output = fixture
            .command()
            .current_dir(directory.as_str().unwrap())
            .env("SHOAL_SPACE", &chat)
            .args(["--json", "rename", name])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", printed(&output));
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["id"].clone()
    };

    let renamed = rename_in(&opened["path"], "Child work");
    assert_eq!(renamed, opened["id"]);
    let renamed = rename_in(&deeper["path"], "Nested work");
    assert_eq!(renamed, deeper["id"]);
    // A workspace not below the chat leaves the command to the chat.
    let renamed = rename_in(&other["path"], "replanning");
    assert_eq!(renamed, chat.as_str());
    assert_eq!(
        fixture.ok(&["status", other["id"].as_str().unwrap()])["workspace"]["name"],
        "other"
    );
    for agent in [&mut agent, &mut nested_agent] {
        agent.kill().unwrap();
        agent.wait().unwrap();
    }
}

/// Start each chat `(checkout, name, session)` names in that checkout, its
/// agent running in the zmx session `session`, if any, of a zmx that labels
/// it, and return their agents and IDs.
fn running_chats(
    fixture: &Fixture,
    chats: &[(&Path, &str, Option<&str>)],
) -> (Vec<std::process::Child>, Vec<String>) {
    let home = fixture.root.path();
    let sessions: String = (chats.iter())
        .filter_map(|(_, _, session)| *session)
        .map(|name| format!("  name={name}\tpid=1\tclients=0\n"))
        .collect();
    fs::write(home.join("zmx-sessions"), sessions).unwrap();
    install_test_script(home.join("bin/zmx"), terminal_zmx());
    chats
        .iter()
        .map(|(checkout, name, session)| {
            fixture.run_chat_with(name, &[], |command| {
                command.current_dir(checkout);
                if let Some(session) = session {
                    command.env("ZMX_SESSION", session);
                }
            })
        })
        .unzip()
}

/// Start the chat `name` in the fixture's repository, its agent running.
pub(crate) fn start_test_chat(fixture: &Fixture, name: &str) -> (std::process::Child, String) {
    fixture.run_chat(name, &[])
}

/// Status names a chat in the namespace it shares with workspaces and
/// leaves out what needs a worktree; workspace processes see neither the
/// chat nor another workspace.
#[test]
fn status_shows_a_chat_without_the_parts_a_worktree_has() {
    let fixture = Fixture::new();
    let own = fixture.add("own");
    let (mut agent, chat) = start_test_chat(&fixture, "planning");

    let status = fixture.ok(&["status", &fixture.id("planning")]);
    assert_eq!(status["kind"], "chat", "{status}");
    assert_eq!(status["chat"]["id"], chat.as_str(), "{status}");
    assert_eq!(status["sessions"], serde_json::json!([]), "{status}");
    for worktree in ["workspace", "executions", "ports", "diff"] {
        assert_eq!(status.get(worktree), None, "{worktree}: {status}");
    }
    // Its running agent keeps it from cleanup, as it would a workspace.
    assert_eq!(
        status["cleanup_reason"]["reason"], "agent_running",
        "{status}"
    );

    let status = fixture.ok(&["status", &fixture.id("own")]);
    assert_eq!(status["kind"], "workspace", "{status}");
    assert_eq!(status["workspace"]["id"], own["id"], "{status}");
    assert_eq!(status["setup_finished"], true, "{status}");

    fixture.add("other");
    for other in ["planning", "other"] {
        let output = fixture.run(&[
            "run",
            "--workspace",
            &fixture.id("own"),
            "--",
            support::shoal_binary(),
            "status",
            other,
        ]);
        assert!(!output.status.success(), "{other}: {}", printed(&output));
    }
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn the_inbox_acts_on_a_chats_ready_row_as_a_chat() {
    let fixture = Fixture::new();
    let (mut agent, _) = start_test_chat(&fixture, "planning");
    fixture.ok(&["ready", "--space", &fixture.id("planning")]);
    // Presses $KEY on the first row whose ID starts with $PICK.
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        r#"#!/bin/sh
cat > "$HOME/picker-input"
printf '%s\n' "$KEY"
awk -F '\t' -v pick="$PICK" 'index($1, pick) == 1 { print; exit }' "$HOME/picker-input"
"#,
    );
    let press = |key: &str| {
        let mut command = fixture.command();
        command.arg("inbox").env("PICK", "ready:").env("KEY", key);
        fixture.terminal_command(&mut command, "", true)
    };
    // Keys that act only on workspaces say so instead of failing to find one.
    let (output, transcript) = press("ctrl-o");
    assert!(!output.status.success(), "{transcript}");
    assert!(
        transcript.contains("this action acts only on workspaces"),
        "{transcript}"
    );
    // Viewing reads the chat's own links.
    let (output, transcript) = press("ctrl-v");
    assert!(!output.status.success(), "{transcript}");
    assert!(
        transcript.contains("the chat links no issue or PR"),
        "{transcript}"
    );
    // Enter attaches to the chat, whose agent says what is ready.
    let (_, transcript) = press("");
    assert!(!transcript.contains("unknown workspace"), "{transcript}");
    assert!(!transcript.contains("no workspace"), "{transcript}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn a_chat_records_its_work_done_and_withdraws_it() {
    // Kept, so the chat stays to show its completion.
    let fixture = Fixture::with_config(Some("[cleanup.done]\nenabled = false\n"));
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let report = fixture.root.path().join("report.md");
    fs::write(&report, "plan\n").unwrap();
    // The branch option is a workspace's.
    let output = fixture.run(&["done", &fixture.id("planning"), "--integrated"]);
    assert!(!output.status.success(), "{}", printed(&output));
    // Done again at the same HEAD with nothing new changes nothing.
    fixture.ok(&["done", &fixture.id("planning")]);
    let output = fixture.run(&["done", &fixture.id("planning")]);
    assert!(!output.status.success(), "{}", printed(&output));
    let completion = fixture.ok(&[
        "done",
        &fixture.id("planning"),
        "--attach",
        report.to_str().unwrap(),
    ]);
    assert_eq!(completion["cleanup"], false, "{completion}");
    assert_eq!(
        completion["results"][0]["name"], "report.md",
        "{completion}"
    );
    // The chat keeps running, finished, and the user hears of it.
    let finished = fixture.ok(&["list", "--chats", "--status", "finished"]);
    assert!(finished.to_string().contains("planning"), "{finished}");
    let inbox = fixture.ok(&["inbox"]);
    assert_eq!(inbox[0]["kind"], "chat_done", "{inbox}");
    let alerts = fixture.ok(&["alerts"]);
    assert!(
        alerts
            .as_array()
            .unwrap()
            .iter()
            .any(|alert| { alert["kind"] == "workspace_done" && alert["workspace"] == "planning" }),
        "{alerts}"
    );
    // The user attached the report; the chat's agent attaches its own.
    let files = fixture.ok(&["files", &fixture.id("planning")]);
    assert_eq!(files[0]["name"], "report.md", "{files}");
    assert_eq!(files[0]["attached_by"]["kind"], "user", "{files}");
    let notes = fixture.root.path().join("notes.md");
    fs::write(&notes, "notes\n").unwrap();
    let output = fixture
        .command()
        .args(["--json", "done", "--attach", notes.to_str().unwrap()])
        .env("SHOAL_SPACE", &chat)
        .env("SHOAL_SCOPE_TOKEN", fixture.chat_scope(&chat))
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    let files = fixture.ok(&["files", &fixture.id("planning")]);
    assert_eq!(files[1]["name"], "notes.md", "{files}");
    assert_eq!(files[1]["attached_by"]["kind"], "agent", "{files}");
    let completion: Value = serde_json::from_slice(&output.stdout).unwrap();
    let withdrawn = fixture.ok(&["done", "--undo", &fixture.id("planning")]);
    assert_eq!(withdrawn["withdrawn"]["results"], completion["results"]);
    let finished = fixture.ok(&["list", "--chats", "--status", "finished"]);
    assert!(!finished.to_string().contains("planning"), "{finished}");
    let output = fixture.run(&["done", "--undo", &fixture.id("planning")]);
    assert!(!output.status.success(), "{}", printed(&output));
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn completion_cleanup_removes_a_done_chat_with_the_workspaces_below_it() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let output = fixture
        .command()
        .args([
            "--json",
            "start",
            "--wait",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "opened",
        ])
        .env("SHOAL_SPACE", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));

    let completion = fixture.ok(&["done", &fixture.id("planning")]);
    assert_eq!(completion["cleanup"], true, "{completion}");
    // The user hears once the removal finished.
    wait_until("completion cleanup to remove the chat", || {
        let alerts = fixture.ok(&["alerts", "--all"]);
        alerts
            .as_array()
            .unwrap()
            .iter()
            .any(|alert| alert["kind"] == "workspace_removed" && alert["workspace"] == "planning")
    });
    assert_eq!(fixture.ok(&["list", "--chats"]), serde_json::json!([]));
    // The workspace below it went first, and its agent stopped.
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    // Its wrapper ends once it stopped the agent.
    agent.wait().unwrap();
}

/// A hold keeps a chat from cleanup, as it keeps a workspace, until it is
/// released; what needs a worktree refuses a chat.
#[test]
fn a_hold_keeps_a_stopped_chat_until_it_is_released() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "notes");
    fixture.end_chat_agent(&chat);
    agent.wait().unwrap();
    let space = fixture.id("notes");
    let held = fixture.ok(&[
        "acquire",
        "hold",
        "--space",
        &space,
        "--name",
        "quay-thread",
    ]);
    assert_eq!(held["space_id"], chat.as_str(), "{held}");
    // Without --space, a command run in the chat lists the chat's own.
    let listed = fixture
        .command()
        .args(["--json", "leases", "hold"])
        .env("SHOAL_SPACE", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(listed.status.success(), "{}", printed(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0]["name"], "quay-thread", "{listed}");
    let status = fixture.ok(&["status", &space]);
    assert_eq!(
        status["chat"]["holds"][0]["name"], "quay-thread",
        "{status}"
    );
    assert_eq!(status["cleanup_reason"]["reason"], "held", "{status}");
    let preview = fixture.ok(&["cleanup", "--dry-run"]);
    assert_eq!(
        preview["kept"],
        serde_json::json!([{"space": "notes", "reason": {"reason": "held"}}]),
        "{preview}"
    );
    let report = fixture.ok(&["cleanup", "spaces"]);
    assert_eq!(report["removed"], serde_json::json!([]), "{report}");
    let port = fixture.run(&["acquire", "port", "web", "--space", &space]);
    assert!(!port.status.success(), "{}", printed(&port));
    // Removing it names the hold it releases before asking.
    let (no, prompt) = fixture.interactive(&["rm", &space], "n\n");
    assert!(!no.status.success());
    assert!(prompt.contains("quay-thread"), "{prompt}");

    fixture.ok(&[
        "release",
        "hold",
        "--space",
        &space,
        "--name",
        "quay-thread",
    ]);
    let report = fixture.ok(&["cleanup", "spaces"]);
    assert_eq!(report["removed"], serde_json::json!(["notes"]), "{report}");
}

/// A fake agent that records its directory and NUL-separated arguments.
pub(crate) fn install_recording_agent(fixture: &Fixture, agent: &str) {
    install_test_script(
        fixture.root.path().join("bin").join(agent),
        "#!/bin/sh\npwd > \"$HOME/chat-cwd\"\nprintf '%s\\0' \"$@\" > \"$HOME/chat-args\"\necho \"${ZMX_SESSION:-none}\" >> \"$HOME/chat-sessions\"\nif [ -z \"$SHOAL_WORKSPACE_PATH\" ]; then echo \"$SHOAL_SPACE\"; fi > \"$HOME/chat-id\"\necho \"$SHOAL_SCOPE_TOKEN\" > \"$HOME/chat-scope\"\necho \"$SHOAL_OUTPUT\" > \"$HOME/chat-output\"\necho \"$SHOAL_WORKSPACE_PATH$SHOAL_WORKSPACE\" > \"$HOME/chat-workspace\"\n",
    );
}

/// The default name of a chat about the fixture's repository.
pub(crate) const CHAT_NAME: &str = "repo with ' quotes & $literal chat";

/// Every retained journal event.
pub(crate) fn journal(fixture: &Fixture) -> Vec<Value> {
    let output = fixture
        .command()
        .args(["--json", "internal", "events"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(crate) fn chat_args(fixture: &Fixture) -> Vec<String> {
    fs::read_to_string(fixture.root.path().join("chat-args"))
        .unwrap()
        .split_terminator('\0')
        .map(str::to_owned)
        .collect()
}

/// `args` after the leading arguments that give an agent the agent
/// directories, once they are checked to name `/tmp`, the default one.
pub(crate) fn after_temp_dirs<S: AsRef<str>>(args: &[S]) -> &[S] {
    let mut directories = Vec::new();
    let mut rest = args;
    loop {
        match rest {
            [flag, directory, tail @ ..] if flag.as_ref() == "--add-dir" => {
                directories.push(directory.as_ref());
                rest = tail;
            }
            [flag, tail @ ..] if flag.as_ref().starts_with("--add-dir=") => {
                directories.push(&flag.as_ref()["--add-dir=".len()..]);
                rest = tail;
            }
            _ => break,
        }
    }
    let tmp = fs::canonicalize("/tmp").unwrap();
    assert!(
        directories.contains(&tmp.to_str().unwrap()),
        "{directories:?}"
    );
    rest
}

/// A heavy chat started from the user's terminal while no slot is free is
/// recorded at once with its agent's entry, and its start names its number.
#[test]
fn a_waiting_chat_from_a_terminal_is_recorded_at_once() {
    let fixture = Fixture::with_config(Some(
        "machine.max_heavy_agents = 1\n[machine.overload.disk]\nenabled = false\n\
         [modes.pair]\nkind = 'chat'\n\
         [agents.waiter]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    ));
    let repo = fixture.repo.to_str().unwrap();
    let mut first = fixture
        .command()
        .args(["start", "--wait", repo, "--branch", "first", "--here"])
        .args(["--agent", "waiter"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the first agent start", || {
        fixture.ok(&["queue"]) == serde_json::json!([])
            && fixture.ok(&["list"])[0]["running"] == true
    });

    let mut start = fixture.command();
    start.args(["start", "--mode", "pair", repo, "--agent", "waiter"]);
    let (output, transcript) = fixture.terminal_command(&mut start, "", true);
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let chats = fixture.ok(&["list", "--chats"]);
    let chat = chats
        .as_array()
        .unwrap()
        .iter()
        .find(|space| space["kind"] == "chat");
    let chat = chat.unwrap_or_else(|| panic!("{chats}"));
    assert!(
        transcript.contains(&format!("Queued chat {},", chat["number"])),
        "{transcript}"
    );
    let entries = fixture.ok(&["queue"]);
    assert_eq!(entries[0]["space_id"], chat["id"], "{entries}");
    fixture.ok(&["stop", &fixture.id("first")]);
    first.wait().unwrap();
}

/// Detaching from a new chat's zmx session names the chat, which its start
/// recorded before the session opened, by its number.
#[test]
fn detaching_from_a_new_chats_session_names_its_number() {
    let fixture = Fixture::new();
    install_recording_agent(&fixture, "claude");
    fs::write(fixture.root.path().join("zmx-attach-detaches"), "").unwrap();
    let mut start = fixture.command();
    start.args(["start", "--mode", "chat", fixture.repo.to_str().unwrap()]);
    start.args(["--agent", "claude"]);
    let (output, transcript) = fixture.terminal_command(&mut start, "", true);
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let chats = fixture.ok(&["list", "--chats"]);
    let number = &chats[0]["number"];
    assert!(
        transcript.contains(&format!("run `shoal attach {number}` to return")),
        "{transcript}"
    );
}

#[test]
fn chat_runs_the_agent_in_the_checkout_in_a_labelled_zmx_session() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    install_recording_agent(&fixture, "claude");
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let id = repository["id"].as_str().unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();

    // The chat starts under its default name without asking.
    let launch = |command: &mut Command| fixture.terminal_command(command, "", true);
    // A person in a JSON-default environment chats with `--text`, which the
    // session's Shoal uses too, since a chat needs text on a terminal.
    let (output, transcript) = launch(fixture.command().env("SHOAL_OUTPUT", "json").args([
        "--text",
        "start",
        "--mode",
        "chat",
        fixture.repo.to_str().unwrap(),
        "--agent",
        "claude",
        "--",
        "--model",
        "fast",
    ]));
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let calls = zmx_calls(&fixture);
    assert_eq!(calls[0], ["list"]);
    // The session takes an opaque name, since nothing is built from the
    // chat's name.
    assert_eq!(calls[1][..3], ["attach", "--labels", "shoal.created=1"]);
    let session = &calls[1][3];
    assert!(
        session.starts_with("chat-") && session.len() == "chat-".len() + 8,
        "{session}"
    );
    // The start records the chat under the name chosen here before the
    // session opens; the session's Shoal runs that chat's agent, labels its
    // session with it and becomes its agent.
    let started = chat_created(&fixture);
    assert_eq!(started["type"], "event");
    assert_eq!(started["repository_id"], id);
    assert_eq!(started["name"], CHAT_NAME);
    assert_eq!(started.get("path"), None, "{started}");
    let chat_id = started["space_id"].as_str().unwrap();
    let chat = calls[1].iter().position(|arg| arg == "start").unwrap();
    assert_eq!(calls[1][chat - 1], "--text");
    assert_eq!(
        calls[1][chat..],
        [
            "start",
            id,
            "--mode",
            "chat",
            "--as-chat",
            "--here",
            "--agent",
            "claude",
            "--name",
            CHAT_NAME,
            "--chat",
            chat_id,
            "--",
            "--model",
            "fast"
        ]
    );
    // It tells its session apart from a later one under the name before
    // recording the chat there.
    assert_eq!(calls[2], ["list"]);
    assert_eq!(
        calls[3][..4],
        [
            "set",
            session,
            &format!("shoal.chat={chat_id}"),
            "shoal.workspace="
        ]
    );
    assert_eq!(
        fs::read_to_string(home.join("chat-id")).unwrap().trim(),
        chat_id
    );
    assert_eq!(
        fs::read_to_string(home.join("chat-output")).unwrap().trim(),
        "json"
    );
    assert_eq!(
        fs::read_to_string(home.join("chat-cwd")).unwrap().trim(),
        checkout.to_str().unwrap()
    );
    let args = chat_args(&fixture);
    assert_eq!(args[0], "--append-system-prompt");
    assert!(args[1].contains(checkout.to_str().unwrap()), "{}", args[1]);
    // The chat's hooks deliver its messages, report its turn state and
    // wake it, without the hooks that need a worktree.
    assert_eq!(args[2], "--settings");
    let hooks: Value = serde_json::from_str(&args[3]).unwrap();
    let commands = |event: &str| -> Vec<String> {
        (hooks["hooks"][event].as_array().unwrap().iter())
            .map(|group| group["hooks"][0]["command"].as_str().unwrap().to_owned())
            .collect()
    };
    let runs = |event: &str, text: &str| commands(event).iter().any(|c| c.contains(text));
    assert!(runs("PostToolUse", " messages --hook"));
    assert!(runs("PostToolUse", " internal agent-state working "));
    assert!(runs("SessionStart", " internal agent-state idle "));
    assert!(runs("Stop", " internal agent-state idle --ends-turn "));
    assert!(runs("Stop", " internal wake "));
    assert!(hooks["hooks"].get("WorktreeCreate").is_none(), "{hooks}");
    assert_eq!(after_temp_dirs(&args[4..]), ["--model", "fast"]);
    // The checkout is trusted as workspace launches trust theirs.
    let claude: Value =
        serde_json::from_str(&fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        claude["projects"][checkout.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
    // Chats are not workspace executions: the list has the chat alone.
    let listed = fixture.ok(&["list"]);
    let kinds: Vec<_> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|space| &space["kind"])
        .collect();
    assert_eq!(kinds, ["chat"], "{listed}");

    // Inside a session the agent runs in place, labelling that session, and
    // a later chat starts another one instead of reusing it.
    let before = zmx_calls(&fixture).len();
    let (output, transcript) = launch(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .args([
                "start",
                "--mode",
                "chat",
                "--agent",
                "claude",
                "--name",
                "outer-chat",
            ]),
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let calls = zmx_calls(&fixture);
    assert_eq!(calls[before], ["list"]);
    assert_eq!(calls[before + 1][..2], ["set", "outer"]);
    assert!(
        !calls[before..].iter().any(|call| call[0] == "attach"),
        "{calls:?}"
    );
    assert_eq!(
        fs::read_to_string(home.join("chat-sessions")).unwrap(),
        format!("{session}\nouter\n")
    );
}

#[test]
fn a_chat_replaces_workspace_ownership_in_a_reused_zmx_session() -> anyhow::Result<()> {
    let fixture = Fixture::new();
    let workspace = fixture.add("old-owner");
    let workspace_id = workspace["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("workspace id"))?;
    let home = fixture.root.path();
    install_recording_agent(&fixture, "claude");
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    fs::write(home.join("zmx-keep-sessions"), "")?;
    fs::write(
        home.join("zmx-sessions"),
        format!(
            "name=reused\tpid=1\tshoal.created=1\tshoal.workspace={workspace_id}\tshoal.demo=old-launch\tuser.label=keep\n"
        ),
    )?;
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "reused")
            .args([
                "start", "--mode", "chat", "--here", "--name", "new-chat", "--agent", "claude",
            ]),
        "",
        true,
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let sessions = fs::read_to_string(home.join("zmx-sessions"))?;
    assert!(sessions.contains("shoal.chat="), "{sessions}");
    assert!(sessions.contains("user.label=keep"), "{sessions}");
    assert!(!sessions.contains("shoal.workspace="), "{sessions}");
    assert!(!sessions.contains("shoal.demo="), "{sessions}");
    fixture.ok(&["rm", "--wait", &fixture.id("old-owner"), "--yes"]);
    fixture.ok(&["cleanup"]);
    assert!(
        !zmx_calls(&fixture)
            .iter()
            .any(|call| call.first().is_some_and(|command| command == "kill"))
    );
    Ok(())
}

#[test]
fn workspaces_belong_to_the_chat_that_opened_them() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    // The chat's agent opens a workspace while the chat runs.
    install_test_script(
        home.join("bin/claude"),
        format!(
            "#!/bin/sh\nshoal='{}'\n\"$shoal\" --json start \"$PWD\" --branch chat-worker </dev/null > \"$HOME/chat-add\"\n\"$shoal\" --text list > \"$HOME/chat-work\"\n\"$shoal\" --json list --chats > \"$HOME/chat-chats\"\nprintf '%s' \"$TMPDIR\" > \"$HOME/chat-tmpdir\"\n",
            support::shoal_binary()
        ),
    );
    // The agent's commands reach the daemon of an explicit state directory.
    let state = home.join("state");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .env_remove("SHOAL_STATE_DIR")
            .arg("--state-dir")
            .arg(&state)
            .args(["start", "--mode", "chat", "--agent", "claude"]),
        "\n",
        true,
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let started = chat_created(&fixture);
    let chat = &started["space_id"];
    let added: Value =
        serde_json::from_str(&fs::read_to_string(home.join("chat-add")).unwrap()).unwrap();
    assert_eq!(added["parent"]["id"], *chat, "{added}");
    assert_eq!(added["parent"]["kind"], "chat", "{added}");
    // Its creation names the chat as its parent.
    let created = journal(&fixture)
        .into_iter()
        .find(|event| event["kind"] == "created" && event["name"] == added["name"])
        .unwrap();
    assert_eq!(created["parent"]["id"], *chat, "{created}");

    // While it runs, `list` shows the chat with its workspaces under it.
    let chats: Value =
        serde_json::from_str(&fs::read_to_string(home.join("chat-chats")).unwrap()).unwrap();
    assert_eq!(chats.as_array().map(Vec::len), Some(1), "{chats}");
    let listed = &chats[0];
    assert_eq!(
        [&listed["id"], &listed["name"], &listed["agent"]],
        [chat, &started["name"], &Value::from("claude")]
    );
    // The agent ran with its own temporary directory, which the listing names.
    let tmp_dir = fs::read_to_string(home.join("chat-tmpdir")).unwrap();
    assert!(
        tmp_dir.ends_with(&format!("/.tmp/{}", chat.as_str().unwrap())),
        "{tmp_dir}"
    );
    assert_eq!(listed["tmp_dir"], tmp_dir.as_str());
    assert!(listed["parent"].is_null(), "{listed}");
    assert_eq!(listed["running"], true);
    assert_eq!(
        listed["children"],
        serde_json::json!([{
            "id": added["id"], "name": "chat-worker", "kind": "workspace",
            "branch": added["branch"],
        }])
    );
    // `list` shows the workspace under its chat.
    let rows = fs::read_to_string(home.join("chat-work")).unwrap();
    let rows: Vec<&str> = rows.lines().collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].contains(CHAT_NAME), "{rows:?}");
    assert!(rows[1].contains("└ chat-worker"), "{rows:?}");

    // Once its agent exits the chat stops, keeping its workspace.
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(chats[0]["running"], false, "{chats}");
    assert_eq!(fixture.ok(&["list"])[0]["parent"]["id"], chats[0]["id"]);
}

#[test]
fn removing_a_chat_removes_the_workspaces_below_it_and_stops_its_agent() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    // The chat's agent opens a workspace, and creates a child below it.
    let output = fixture
        .command()
        .args([
            "--json",
            "start",
            "--wait",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "opened",
        ])
        .env("SHOAL_SPACE", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    fixture.ok(&[
        "create",
        "--branch",
        "below",
        "--parent",
        &fixture.id("opened"),
    ]);
    // Another chat runs in the opened workspace's worktree.
    let opened = fixture.id("opened");
    let (mut resident, _) = fixture.run_chat("reading", &["--in", &opened]);

    // Removing the chat asks first, naming what it takes.
    let (output, prompt) = fixture.interactive(&["rm", "--wait", &fixture.id("planning")], "n\n");
    assert!(!output.status.success(), "{prompt}");
    for name in ["opened", "reading", "below"] {
        assert!(prompt.contains(name), "{name}: {prompt}");
    }

    // Naming the resident chat too removes it once, with the chat above it.
    let removed = fixture.ok(&[
        "rm",
        "--wait",
        &fixture.id("planning"),
        &fixture.id("reading"),
        "--yes",
    ]);
    assert_eq!(removed["failed"], serde_json::json!([]), "{removed}");
    let spaces = removed["removed"].as_array().unwrap();
    assert_eq!(spaces.len(), 1, "{removed}");
    assert_eq!(spaces[0]["space"], "planning", "{removed}");
    assert_eq!(
        spaces[0]["children"],
        serde_json::json!(["reading", "below", "opened"])
    );
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    assert_eq!(fixture.ok(&["list", "--chats"]), serde_json::json!([]));
    // Their wrappers end once they stopped their agents.
    agent.wait().unwrap();
    resident.wait().unwrap();
}

#[test]
fn a_workspace_a_chat_opens_is_below_it_and_in_its_repository() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let add = |repository: &Path, name: &str| {
        fixture
            .command()
            .args([
                "--json",
                "start",
                "--wait",
                repository.to_str().unwrap(),
                "--branch",
                name,
            ])
            .env("SHOAL_SPACE", &chat)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let output = add(&fixture.repo, "opened");
    assert!(output.status.success(), "{}", printed(&output));
    fixture.ok(&[
        "create",
        "--branch",
        "below",
        "--parent",
        &fixture.id("opened"),
    ]);

    // One parent link names the chat or the workspace above, of either kind.
    let opened = fixture.ok(&["status", &fixture.id("opened")]);
    assert_eq!(opened["workspace"]["parent"]["id"], chat.as_str());
    assert_eq!(opened["workspace"]["parent"]["kind"], "chat");
    assert_eq!(opened["workspace"]["children"][0]["name"], "below");
    let chats = fixture.ok(&["list", "--chats"]);
    let listed = chats
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == chat.as_str());
    assert_eq!(listed.unwrap()["children"][0]["name"], "opened", "{chats}");

    // A child is in its parent's repository.
    let other = repository_with(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let output = add(&other, "foreign");
    assert!(!output.status.success(), "{}", printed(&output));
    // Refused before an agent's launch is planned or queued.
    let output = fixture
        .command()
        .args([
            "--json",
            "start",
            "--wait",
            other.to_str().unwrap(),
            "--branch",
            "queued",
        ])
        .args(["--agent", "missing-agent"])
        .env("SHOAL_SPACE", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{}", printed(&output));
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));
    // Nor is registering another repository offered there.
    let unregistered = repository_with(fixture.root.path(), "third", &[("tracked", "third\n")]);
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args([
                "--text",
                "start",
                "--wait",
                unregistered.to_str().unwrap(),
                "--branch",
                "third-work",
            ])
            .env("SHOAL_SPACE", &chat),
        "y\n",
        true,
    );
    assert!(!output.status.success(), "{transcript}");
    assert!(!transcript.contains("Register"), "{transcript}");
    assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 2);
    let names: Vec<_> = (fixture.ok(&["list"]).as_array().unwrap().iter())
        .map(|workspace| workspace["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(!names.contains(&"foreign".to_owned()), "{names:?}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

/// Run `shoal create` with `args` as the agent of the chat `chat` would, in
/// `dir`, and return the child it created.
fn create_in_chat(fixture: &Fixture, chat: &str, dir: &Path, args: &[&str]) -> Value {
    let output = fixture
        .command()
        .args(["--json", "create"])
        .args(args)
        .env("SHOAL_SPACE", chat)
        .env("SHOAL_SCOPE_TOKEN", fixture.chat_scope(chat))
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn a_chat_creates_child_workspaces_below_itself() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let child = create_in_chat(&fixture, &chat, &fixture.repo, &["--branch", "child"]);
    assert_eq!(child["parent"]["id"], chat.as_str(), "{child}");
    assert_eq!(child["branch"], "child", "{child}");
    // The user names the chat for a workspace at a commit.
    let planning = fixture.id("planning");
    let at_commit = fixture.ok(&["create", "--no-branch", "--parent", &planning]);
    assert_eq!(at_commit["parent"]["id"], chat.as_str(), "{at_commit}");
    assert_eq!(at_commit["branch"], Value::Null, "{at_commit}");
    // Neither queues an agent of its own.
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn a_chat_in_a_workspaces_worktree_creates_below_the_chat() {
    let fixture = Fixture::new();
    let opened = fixture.add("opened");
    let path = PathBuf::from(opened["path"].as_str().unwrap());
    let (mut agent, chat) = fixture.run_chat("reading", &["--in", &fixture.id("opened")]);
    let child = create_in_chat(&fixture, &chat, &path, &["--branch", "child"]);
    assert_eq!(child["parent"]["id"], chat.as_str(), "{child}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn a_chat_receives_the_events_of_the_workers_it_coordinates() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    // The chat's agent opens a workspace, whose agent then reports. The
    // chat's own scope does not reach that agent's reports.
    install_test_script(
        home.join("bin/claude"),
        format!(
            "#!/bin/sh\nshoal='{}'\n\"$shoal\" start \"$PWD\" --branch chat-worker </dev/null >/dev/null\n\"$shoal\" internal agent-state working --space chat-worker 2>/dev/null; echo $? > \"$HOME/chat-report\"\ntouch \"$HOME/worker-opened\"\nwhile [ ! -e \"$HOME/worker-reported\" ]; do sleep 0.1; done\necho '{{\"hook_event_name\":\"PostToolUse\"}}' | \"$shoal\" messages --hook > \"$HOME/chat-hook\"\n\"$shoal\" messages --hook </dev/null > \"$HOME/chat-hook-again\"\n",
            support::shoal_binary()
        ),
    );
    let (output, transcript) = std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            fixture.terminal_command(
                fixture
                    .command()
                    .current_dir(&fixture.repo)
                    .env("ZMX_SESSION", "outer")
                    .args(["start", "--mode", "chat", "--agent", "claude"]),
                "\n",
                true,
            )
        });
        wait_until("worker opened", || home.join("worker-opened").exists());
        // The worker's agent reports, as its own scope would.
        fixture.ok(&[
            "internal",
            "agent-state",
            "working",
            "--space",
            &fixture.id("chat-worker"),
        ]);
        fixture.ok(&[
            "alerts",
            "send",
            "--space",
            &fixture.id("chat-worker"),
            "Tests pass",
        ]);
        fs::write(home.join("worker-reported"), "").unwrap();
        chat.join().unwrap()
    });
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let refused = fs::read_to_string(home.join("chat-report")).unwrap();
    assert_ne!(refused.trim(), "0", "the chat reported its worker's state");
    let hook: Value =
        serde_json::from_str(&fs::read_to_string(home.join("chat-hook")).unwrap()).unwrap();
    let context = hook["hookSpecificOutput"]["additionalContext"].as_str();
    assert!(
        context.is_some_and(|context| context.contains("chat-worker: Tests pass")),
        "{hook}"
    );
    // A delivered message does not come back, and never reached the user.
    assert_eq!(
        fs::read_to_string(home.join("chat-hook-again")).unwrap(),
        ""
    );
    assert_eq!(fixture.ok(&["alerts"]), serde_json::json!([]));
}

#[test]
fn a_chats_agent_is_scoped_to_the_chat_and_the_spaces_below_it() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    fixture.add("outside");
    // Each command's exit status, then what it printed to stderr.
    install_test_script(
        home.join("bin/claude"),
        format!(
            r#"#!/bin/sh
shoal='{}'
run() {{ name=$1; shift; "$shoal" "$@" </dev/null >"$HOME/$name.out" 2>"$HOME/$name.err"; echo $? > "$HOME/$name"; }}
run start --json start --wait "$PWD" --branch below
below=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$HOME/start.out")
run stop stop --interrupt "$below"
run env internal env "$below"
run ownenv internal env
run rename rename renamed
run alerts alerts
run repo repo add "$HOME"
run outside messages send --space {} hello
run takeover start "$PWD" --branch outside
run remove rm "$below" --yes --wait
"#,
            support::shoal_binary(),
            fixture.id("outside")
        ),
    );
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .args([
                "start", "--mode", "chat", "--agent", "claude", "--name", "planning",
            ]),
        "",
        true,
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let status = |name: &str| {
        let code = fs::read_to_string(home.join(name)).unwrap();
        let error = fs::read_to_string(home.join(format!("{name}.err"))).unwrap();
        (code.trim().to_owned(), error)
    };
    // It opens, stops and removes the workspaces below it, exports their
    // environment and its own, and renames itself.
    for allowed in ["start", "stop", "env", "ownenv", "rename", "remove"] {
        let (code, error) = status(allowed);
        assert_eq!(code, "0", "{allowed}: {error}");
    }
    let exported = |name: &str| -> Value {
        serde_json::from_str(&fs::read_to_string(home.join(format!("{name}.out"))).unwrap())
            .unwrap()
    };
    let (below, own) = (exported("env"), exported("ownenv"));
    assert!(below["SHOAL_WORKSPACE_PATH"].is_string(), "{below}");
    assert!(own["SHOAL_SCOPE_TOKEN"].is_string(), "{own}");
    assert!(own["SHOAL_WORKSPACE_PATH"].is_null(), "{own}");
    // It is refused what a workspace's agent is, and what lies outside it.
    for refused in ["alerts", "repo", "outside", "takeover"] {
        let (code, error) = status(refused);
        assert_ne!(code, "0", "{refused}: {error}");
    }
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(chats[0]["name"], "renamed", "{chats}");
    let names: Vec<_> = (fixture.ok(&["list"]).as_array().unwrap().iter())
        .filter(|space| space["kind"] == "workspace")
        .map(|workspace| workspace["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["outside"]);
    assert!(fixture.ok(&["status", &fixture.id("outside")])["workspace"]["parent"].is_null());
}

/// A chat's agent adds a workspace with an agent, which the daemon starts
/// there with the workspace's own scope once the workspace exists.
#[test]
fn a_chats_agent_adds_a_workspace_whose_agent_the_daemon_starts() {
    let fixture = Fixture::with_config(Some("[machine.overload.disk]\nenabled = false\n"));
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    install_test_script(
        home.join("bin/claude"),
        format!(
            "#!/bin/sh\nshoal='{}'\n\"$shoal\" --json start \"$PWD\" --branch worker --agent codex -- 'Take the part' > \"$HOME/add.json\" 2> \"$HOME/add.err\"\nworker=$(head -n 1 \"$HOME/add.json\" | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"id\"])')\n\"$shoal\" start \"$PWD\" --branch later --agent codex --add-blocker \"$worker\" 2> \"$HOME/later.err\"\necho $? > \"$HOME/later\"\n\"$shoal\" start \"$PWD\" --branch now --agent codex --now 2> \"$HOME/now.err\"\necho $? > \"$HOME/now\"\n",
            support::shoal_binary()
        ),
    );
    install_test_script(
        home.join("bin/codex"),
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$HOME/worker.tmp\"\nmv \"$HOME/worker.tmp\" \"$HOME/worker\"\n",
    );
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .args([
                "start", "--mode", "chat", "--agent", "claude", "--name", "planning",
            ]),
        "",
        true,
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    // It reports the workspace, then the agent queued there.
    let added = fs::read_to_string(home.join("add.json")).unwrap();
    let error = fs::read_to_string(home.join("add.err")).unwrap();
    let reports: Vec<Value> = (added.lines())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("{added}\n{error}")))
        .collect();
    assert_eq!(reports.len(), 2, "{added}");
    assert_eq!(reports[0]["name"], "worker", "{added}");
    assert_eq!(reports[1]["agent"], "codex", "{added}");
    let worker = fixture.ok(&["status", &fixture.id("worker")]);
    assert_eq!(
        worker["workspace"]["parent"]["name"], "planning",
        "{worker}"
    );
    let started = home.join("worker");
    wait_until("the worker's agent start", || started.exists());
    let started = fs::read_to_string(&started).unwrap();
    let lines: Vec<_> = started.lines().collect();
    assert_eq!(lines[0], worker["workspace"]["path"], "{started}");
    assert_eq!(lines.last(), Some(&"Take the part"), "{started}");
    // A blocked agent's workspace is the chat's at once, its agent queued
    // there until its blockers clear.
    let error = fs::read_to_string(home.join("later.err")).unwrap();
    let code = fs::read_to_string(home.join("later")).unwrap();
    assert_eq!(code.trim(), "0", "{error}");
    let queue = fixture.ok(&["queue"]);
    let later = (queue.as_array().unwrap().iter())
        .find(|entry| entry["name"] == "later")
        .unwrap_or_else(|| panic!("{queue}"));
    let chat = &fixture.ok(&["list", "--chats"])[0];
    let workspace = fixture.ok(&["status", &fixture.id("later")]);
    assert_eq!(later["opens"], workspace["workspace"]["id"], "{later}");
    assert_eq!(
        workspace["workspace"]["parent"]["id"], chat["id"],
        "{workspace} {chat}"
    );
    // One that would start at once, ignoring the queue, waits for its
    // `limits` approval before its workspace exists.
    let error = fs::read_to_string(home.join("now.err")).unwrap();
    let code = fs::read_to_string(home.join("now")).unwrap();
    assert_eq!(code.trim(), "2", "{error}");
    assert!(
        !fixture
            .run(&["status", &fixture.id("now")])
            .status
            .success()
    );
    // The chat's own session is not relabelled as the worker's.
    let id = worker["workspace"]["id"].as_str().unwrap();
    let relabelled = zmx_calls(&fixture).into_iter().find(|call| {
        call.first().is_some_and(|command| command == "set")
            && call.get(1).is_some_and(|session| session == "outer")
            && call.iter().any(|arg| arg.contains(id))
    });
    assert_eq!(relabelled, None);
}

#[test]
fn a_chats_agent_adds_a_workspace_for_an_issue_it_reads_in_its_chat() {
    let fixture = Fixture::with_config(Some("[machine.overload.disk]\nenabled = false\n"));
    fixture.add_github_origin();
    fixture.script_github(&github_issue(12, "Clarify retries", "Retry twice."));
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    // The chat's scope reads the issue and its blockers only in the chat,
    // never in the repository's registered checkout.
    install_test_script(
        home.join("bin/claude"),
        format!(
            "#!/bin/sh\n'{}' --json start \"$PWD\" --issue 12 --agent codex > \"$HOME/add.json\" 2> \"$HOME/add.err\"\n",
            support::shoal_binary()
        ),
    );
    install_test_script(home.join("bin/codex"), "#!/bin/sh\n");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .args([
                "start", "--mode", "chat", "--agent", "claude", "--name", "planning",
            ]),
        "",
        true,
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let added = fs::read_to_string(home.join("add.json")).unwrap();
    let error = fs::read_to_string(home.join("add.err")).unwrap();
    let report: Value = serde_json::from_str(added.lines().next().unwrap_or_default())
        .unwrap_or_else(|_| panic!("{added}\n{error}"));
    assert_eq!(report["name"], "Clarify retries", "{added}");
    let worker = fixture.ok(&["status", &fixture.id("Clarify retries")]);
    assert_eq!(
        worker["workspace"]["parent"]["name"], "planning",
        "{worker}"
    );
}

#[test]
fn a_workspaces_agent_reads_the_issue_it_adds_or_plans_in_its_workspace() {
    let fixture = Fixture::with_config(Some(
        "[machine.overload.disk]\nenabled = false\n\
         [agents.claude]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    ));
    fixture.add_github_origin();
    fixture.script_github(&github_issue(12, "Clarify retries", "Retry twice."));
    install_test_script(fixture.root.path().join("bin/zmx"), terminal_zmx());
    fixture.add("parent");
    // The workspace's scope reads the issue and its blockers only in its
    // workspace, never in the repository's registered checkout.
    let repo = fixture.repo.to_str().unwrap();
    let added = scoped_command(
        &fixture,
        "parent",
        &["start", "--wait", repo, "--issue", "12"],
    );
    assert!(added.status.success(), "{}", printed(&added));
    let worker = fixture.ok(&["status", &fixture.id("Clarify retries")]);
    assert_eq!(worker["workspace"]["parent"]["name"], "parent", "{worker}");
    // Planning the issue reads it there too.
    let planned = scoped_command(
        &fixture,
        "parent",
        &[
            "start", "--mode", "plan", "--issue", "12", "--agent", "claude",
        ],
    );
    assert!(planned.status.success(), "{}", printed(&planned));
}

/// A chat's agent plans an issue in a chat below its own: both the chat's
/// start and the background place that starts the new chat's agent read the
/// issue.
#[test]
fn a_chats_agent_plans_an_issue_in_a_chat_below_it() {
    let fixture = Fixture::with_config(Some(
        "[machine.overload.disk]\nenabled = false\n\
         [agents.claude]\ncommand = ['sh', './chat-agent.sh']\n",
    ));
    fixture.add_github_origin();
    fixture.script_github(&github_issue(12, "Clarify retries", "Retry twice."));
    let home = fixture.root.path();
    install_test_script(home.join("bin").join("zmx"), terminal_zmx());
    fs::write(
        fixture.repo.join("chat-agent.sh"),
        format!(
            "shoal='{}'\n\
             echo \"$SHOAL_SPACE\" >> \"$HOME/chats-started\"\n\
             if [ ! -e \"$HOME/plan-started\" ]; then\n\
             touch \"$HOME/plan-started\"\n\
             \"$shoal\" start --mode plan --issue 12 --agent claude </dev/null > \"$HOME/plan\" 2>&1\n\
             echo $? > \"$HOME/plan-status\"\n\
             fi\n\
             trap 'exit 0' TERM\n\
             while :; do sleep 1; done\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    let started = fixture
        .command()
        .current_dir(&fixture.repo)
        .args([
            "start", "--mode", "chat", "--agent", "claude", "--name", "planning",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(started.status.success(), "{}", printed(&started));
    let status = wait_for("the planning chat's start of the plan", || {
        (fs::read_to_string(home.join("plan-status")).ok()).filter(|status| status.ends_with('\n'))
    });
    let output = fs::read_to_string(home.join("plan")).unwrap_or_default();
    assert_eq!(status.trim(), "0", "{output}");
    wait_until("the plan's agent", || {
        fs::read_to_string(home.join("chats-started")).is_ok_and(|ids| ids.lines().count() == 2)
    });
    let chats = fixture.ok(&["list", "--chats"]);
    let plan = (chats.as_array().into_iter().flatten())
        .find(|chat| chat["name"] == "Clarify retries")
        .unwrap_or_else(|| panic!("{chats}"));
    assert_eq!(plan["parent"]["name"], "planning", "{chats}");
}

/// A background place may still run below its scoped starter's processes,
/// so it takes the scope token its starter hands it before its own scope is
/// checked, and reports to the starter whatever stops the chat.
#[test]
fn a_background_place_below_its_starter_takes_the_handed_scope() {
    let fixture = Fixture::with_config(Some("[agents.claude]\ncommand = ['true']\n"));
    fixture.add("parent");
    let handoff = fixture.root.path().join("handoff");
    fs::create_dir(&handoff).unwrap();
    // The place's command inherits the workspace's scope through its
    // ancestors, without the token in its own environment.
    let place = fixture
        .command()
        .args([
            "run",
            "--workspace",
            &fixture.id("parent"),
            "--",
            "sh",
            "-c",
        ])
        .arg(
            "printf %s \"$SHOAL_SCOPE_TOKEN\" > \"$HANDOFF/scope\"; unset SHOAL_SCOPE_TOKEN; \
             exec \"$SHOAL\" --json start --mode chat \"$REPO\" --here --name sub --agent claude \
             --handoff \"$HANDOFF\" </dev/null",
        )
        .env("SHOAL", support::shoal_binary())
        .env("REPO", &fixture.repo)
        .env("HANDOFF", &handoff)
        .output()
        .unwrap();
    // Without a terminal the chat does not start, after the place took the
    // token, and the starter learns why.
    assert!(!place.status.success(), "{}", printed(&place));
    assert!(!handoff.join("scope").exists(), "{}", printed(&place));
    let error: Value = serde_json::from_slice(&place.stderr).unwrap();
    let report = fs::read_to_string(handoff.join("report")).unwrap_or_default();
    assert_eq!(
        error["error"]["message"],
        report.as_str(),
        "{}",
        printed(&place)
    );
}

/// `shoal config setup` without a terminal has the daemon start its agent
/// in a chat without a repository, in the setup mode, on the global config,
/// whose directory the agent gets to work in.
#[test]
fn config_setup_without_a_terminal_launches_its_agent_through_the_daemon() {
    let fixture = Fixture::new();
    install_test_script(fixture.root.path().join("bin/zmx"), terminal_zmx());
    // The agent writes its arguments whole, so their file holds them all
    // once it exists.
    install_test_script(
        fixture.root.path().join("bin/claude"),
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$HOME/chat-args.tmp\"\nmv \"$HOME/chat-args.tmp\" \"$HOME/chat-args\"\n",
    );
    let output = fixture
        .command()
        .args(["config", "setup", "--agent", "claude"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    let args = fixture.root.path().join("chat-args");
    wait_until("the setup agent start", || args.exists());
    let args = chat_args(&fixture);
    // As agents compare paths, through links such as macOS's `/tmp`.
    let config_dir = fs::canonicalize(fixture.root.path().join(".config/shoal")).unwrap();
    let given = format!("--add-dir={}", config_dir.display());
    assert!(args.contains(&given), "{args:?}");
    let prompt = args.last().unwrap();
    let config = config_dir.join("config.toml");
    assert!(prompt.contains(config.to_str().unwrap()), "{prompt}");
    let chats = fixture.ok(&["list", "--chats"]);
    let chat = &chats[0];
    assert_eq!(chat["mode"], "setup", "{chats}");
    assert_eq!(chat["repository_id"], Value::Null, "{chats}");
    wait_zmx_sessions_ended(fixture.root.path());
}

/// A scoped caller's `start --here` runs a chat below its space in its own
/// terminal, as the chat's agent with the chat's scope.
#[test]
fn a_scoped_start_runs_a_chat_here_as_its_own() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './chat-agent.sh']\n",
    ));
    let agent = r#"echo "$SHOAL_SPACE" "$SHOAL_SCOPE_TOKEN" > "$HOME/chat-env""#;
    fs::write(fixture.repo.join("chat-agent.sh"), agent).unwrap();
    fixture.add("parent");
    let parent = fixture.id("parent");
    let caller = "echo \"$SHOAL_SCOPE_TOKEN\" > \"$HOME/caller-token\"; \
                  exec \"$SHOAL\" start --mode chat --here --agent claude --name here";
    let (output, transcript) = fixture.interactive_command(
        (fixture.command().current_dir(&fixture.repo))
            .args(["run", "--workspace", &parent, "--", "sh", "-c", caller])
            .env("SHOAL", support::shoal_binary()),
        "",
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let chats = fixture.ok(&["list", "--chats"]);
    let [chat] = chats.as_array().unwrap().as_slice() else {
        panic!("{chats}");
    };
    assert_eq!(chat["parent"]["id"], parent.as_str(), "{chat}");
    let recorded = fs::read_to_string(fixture.root.path().join("chat-env")).unwrap();
    let [id, token] = recorded.split_whitespace().collect::<Vec<_>>()[..] else {
        panic!("{recorded:?}");
    };
    assert_eq!(id, chat["id"], "{chat}");
    let caller_token = fs::read_to_string(fixture.root.path().join("caller-token")).unwrap();
    assert!(
        !token.is_empty() && token != caller_token.trim(),
        "{recorded:?}"
    );
}

#[test]
fn any_space_opens_any_kind_of_child() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './chat-agent.sh']\n",
    ));
    let home = fixture.root.path();
    install_test_script(home.join("bin").join("zmx"), terminal_zmx());
    // The first chat's agent starts a chat below its own; each records its
    // chat and keeps running.
    fs::write(
        fixture.repo.join("chat-agent.sh"),
        format!(
            "shoal='{}'\n\
             echo \"$SHOAL_SPACE\" >> \"$HOME/chats-started\"\n\
             if [ ! -e \"$HOME/nested-started\" ]; then\n\
             touch \"$HOME/nested-started\"\n\
             \"$shoal\" start --mode chat --agent claude --name nested </dev/null > \"$HOME/nested-chat\" 2>&1\n\
             echo $? > \"$HOME/nested-status\"\n\
             fi\n\
             trap 'exit 0' TERM\n\
             while :; do sleep 1; done\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    fixture.add("parent");
    // A workspace's agent opens a workspace below its own...
    let repo = fixture.repo.to_str().unwrap();
    let below = scoped_command(
        &fixture,
        "parent",
        &["start", "--wait", repo, "--branch", "below"],
    );
    assert!(below.status.success(), "{}", printed(&below));
    let parent = |space: &Value| space["parent"]["name"].as_str().map(str::to_owned);
    let status = fixture.ok(&["status", &fixture.id("below")]);
    assert_eq!(parent(&status["workspace"]).as_deref(), Some("parent"));
    // ...but not a chat in the worktree of a workspace outside its tree...
    fixture.add("other");
    let args = ["start", "--in", &fixture.id("other"), "--agent", "claude"];
    let outside = scoped_command(&fixture, "parent", &args);
    assert!(!outside.status.success(), "{}", printed(&outside));
    // ...rather a chat below it, which opens in the background, as its agent
    // cannot hand over a terminal; that chat's agent opens one below its own.
    // The process returns once the chat holds its own scope, which outlasts
    // the process's.
    let sub = scoped_command(
        &fixture,
        "parent",
        &[
            "start", "--mode", "chat", "--agent", "claude", "--name", "sub",
        ],
    );
    assert!(sub.status.success(), "{}", printed(&sub));
    // The nested start, like sub's, returns once its chat holds its own
    // scope, or says why it did not.
    let status = wait_for("sub's agent's start of nested", || {
        (fs::read_to_string(home.join("nested-status")).ok())
            .filter(|status| status.ends_with('\n'))
    });
    let output = fs::read_to_string(home.join("nested-chat")).unwrap_or_default();
    assert_eq!(status.trim(), "0", "{output}");
    wait_until("nested's agent", || {
        fs::read_to_string(home.join("chats-started")).is_ok_and(|ids| ids.lines().count() == 2)
    });
    let chats = fixture.ok(&["list", "--chats"]);
    let chat = |name: &str| {
        let found = chats
            .as_array()
            .unwrap()
            .iter()
            .find(|chat| chat["name"] == name);
        found.unwrap_or_else(|| panic!("{name}: {chats}")).clone()
    };
    assert_eq!(parent(&chat("sub")).as_deref(), Some("parent"), "{chats}");
    assert_eq!(chat("sub")["parent"]["kind"], "workspace");
    assert_eq!(parent(&chat("nested")).as_deref(), Some("sub"), "{chats}");
    assert_eq!(chat("nested")["parent"]["kind"], "chat");
    // Removing the workspace takes every space below it, deepest first.
    let removed = fixture.ok(&["rm", "--wait", &fixture.id("parent"), "--yes"]);
    let children: Vec<_> = (removed["children"].as_array().unwrap().iter())
        .map(|name| name.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(children.len(), 3, "{removed}");
    for name in ["below", "sub", "nested"] {
        assert!(
            children.iter().any(|child| child == name),
            "{name}: {removed}"
        );
    }
    assert!(
        children.iter().position(|child| child == "nested")
            < children.iter().position(|child| child == "sub")
    );
    assert_eq!(fixture.ok(&["list", "--chats"]), serde_json::json!([]));
}

/// A workspace's agent starts a chat in its own worktree, which the daemon
/// starts below that workspace in a background place.
#[test]
fn a_workspace_agent_starts_a_chat_in_its_own_worktree() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    ));
    install_test_script(fixture.root.path().join("bin").join("zmx"), terminal_zmx());
    fixture.add("parent");
    let parent = fixture.id("parent");
    let args = [
        "start", "--in", &parent, "--mode", "chat", "--agent", "claude", "--name", "reader",
    ];
    let reader = scoped_command(&fixture, "parent", &args);
    assert!(reader.status.success(), "{}", printed(&reader));
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(chats[0]["parent"]["id"], parent.as_str(), "{chats}");
    assert_eq!(
        chats[0]["workdir"]["workspace"]["id"],
        parent.as_str(),
        "{chats}"
    );
}

/// A chat between two workspaces decides for and coordinates the workspace
/// below it, whose process reaches nothing above, and goes with the
/// workspace above it.
#[test]
fn a_chat_between_workspaces_decides_and_coordinates_for_the_one_below() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', '-c', 'exec sh \"$HOME/agent.sh\"']\n",
    ));
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    let home = fixture.root.path();
    install_test_script(home.join("bin").join("zmx"), terminal_zmx());
    // The chat's agent opens a workspace below its chat and keeps running;
    // that workspace's agent asks for a port that needs approval.
    fs::write(
        home.join("agent.sh"),
        format!(
            "shoal='{}'\n\
             if [ -n \"$SHOAL_WORKSPACE_PATH\" ]; then\n\
             \"$shoal\" --json acquire port web --reason 'serve preview' > \"$HOME/asked.tmp\" 2>&1\n\
             mv \"$HOME/asked.tmp\" \"$HOME/asked\"\n\
             while [ ! -e \"$HOME/release\" ]; do sleep 0.1; done\n\
             exit 0\n\
             fi\n\
             \"$shoal\" start --wait \"$PWD\" --branch deep </dev/null > \"$HOME/deep\" 2>&1\n\
             touch \"$HOME/chat-ready\"\n\
             trap 'exit 0' TERM\n\
             while :; do sleep 1; done\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    fixture.add("top");
    let middle = scoped_command(
        &fixture,
        "top",
        &[
            "start", "--mode", "chat", "--agent", "claude", "--name", "middle",
        ],
    );
    assert!(middle.status.success(), "{}", printed(&middle));
    wait_until("the chat's workspace", || home.join("chat-ready").exists());
    let deep = fixture.ok(&["status", &fixture.id("deep")]);
    assert_eq!(deep["workspace"]["parent"]["name"], "middle", "{deep}");
    let chats = fixture.ok(&["list", "--chats"]);
    let middle = chats[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(chats[0]["parent"]["name"], "top", "{chats}");

    // The running chat, not the agentless workspace above it, decides what
    // the agent of the workspace below asks for.
    let mut worker = fixture
        .command()
        .args([
            "start",
            "--in",
            &fixture.id("deep"),
            "--here",
            "--agent",
            "claude",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the worker's request", || home.join("asked").exists());
    let asked: Value =
        serde_json::from_str(&fs::read_to_string(home.join("asked")).unwrap()).unwrap();
    let request = fixture.ok(&["approvals", asked["request"]["number"].as_str().unwrap()]);
    assert_eq!(request["decider"], middle.as_str(), "{request}");
    // The chat coordinates it: the workspace's activity reaches the chat.
    fixture.ok(&[
        "alerts",
        "send",
        "--space",
        &fixture.id("deep"),
        "Tests pass",
    ]);
    let messages = fixture.ok(&["messages", &middle]);
    assert!(messages.to_string().contains("Tests pass"), "{messages}");
    // The workspace's process reaches neither the chat nor the workspace
    // above it.
    for space in ["middle", "top"] {
        let refused = scoped_command(
            &fixture,
            "deep",
            &["messages", "send", "--space", &fixture.id(space), "hi"],
        );
        assert!(!refused.status.success(), "{space}: {}", printed(&refused));
    }
    fs::write(home.join("release"), "").unwrap();
    assert!(worker.wait().unwrap().success());
    // Removing the top workspace takes the chat and the workspace below it.
    let removed = fixture.ok(&["rm", "--wait", &fixture.id("top"), "--yes"]);
    assert_eq!(
        removed["children"],
        serde_json::json!(["deep", "middle"]),
        "{removed}"
    );
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    assert_eq!(fixture.ok(&["list", "--chats"]), serde_json::json!([]));
}

#[test]
fn chat_instructions_append_the_chat_rules_to_shoals_description() {
    let fixture = Fixture::new();
    install_recording_agent(&fixture, "claude");
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(config_dir.join("rules")).unwrap();
    fs::write(config_dir.join("rules/chat.md"), "Discuss {repo} at {path}").unwrap();
    fs::create_dir_all(fixture.repo.join(".shoal/rules")).unwrap();
    fs::write(
        fixture.repo.join(".shoal/rules/chat.md"),
        "Plans live in docs {branch}",
    )
    .unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    let chat = || {
        let (output, transcript) = fixture.terminal_command(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .env("ZMX_SESSION", "outer")
                .args(["start", "--mode", "chat", "--agent", "claude"]),
            "\n",
            true,
        );
        assert!(
            output.status.success(),
            "{}\n{transcript}",
            printed(&output)
        );
        chat_args(&fixture)
    };
    let name = checkout.file_name().unwrap().to_str().unwrap();
    let args = chat();
    assert_eq!(args[0], "--append-system-prompt");
    let description = format!(
        "You are chatting with the user about the {name} repository in its registered\n\
         checkout at {}.",
        checkout.display()
    );
    assert!(args[1].starts_with(&description), "{}", args[1]);
    assert!(
        args[1].ends_with(&format!(
            "\n\nDiscuss {name} at {}\n\nPlans live in docs {{branch}}",
            checkout.display()
        )),
        "{}",
        args[1]
    );
    // Without additions, Shoal's facts, space rules and `work` rule stand alone.
    fs::write(config_dir.join("rules/chat.md"), "").unwrap();
    fs::write(fixture.repo.join(".shoal/rules/chat.md"), "").unwrap();
    let args = chat();
    assert!(args[1].starts_with(&description), "{}", args[1]);
    assert_eq!(
        guidance_end(&args[1], 0),
        Some(args[1].len()),
        "{}",
        args[1]
    );
}

/// A chat in a workspace runs its agent in the worktree as a chat, told
/// not to edit; workspace commands it runs act on the workspace, and
/// removing the workspace lists and removes the chat first.
#[test]
fn a_chat_runs_in_a_workspaces_worktree_and_goes_with_it() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    install_recording_agent(&fixture, "claude");
    fixture.add("worker");
    let worktree = fs::canonicalize(
        fixture.ok(&["status", &fixture.id("worker")])["workspace"]["path"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    // Without a value, the flag takes the current workspace. The chat takes
    // none of the identity a shell Shoal opened there carries.
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&worktree)
            .env("ZMX_SESSION", "outer")
            .env("SHOAL_SPACE", "worker-id")
            .env("SHOAL_WORKSPACE_PATH", &worktree)
            .env("SHOAL_WORKSPACE", "worker")
            .args(["start", "--in", "--mode", "chat", "--agent", "claude"]),
        "\n",
        true,
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(
        fs::read_to_string(home.join("chat-cwd")).unwrap().trim(),
        worktree.to_str().unwrap()
    );
    assert_eq!(
        fs::read_to_string(home.join("chat-workspace")).unwrap(),
        "\n"
    );
    let args = chat_args(&fixture);
    assert_eq!(args[0], "--append-system-prompt");
    assert!(args[1].contains("worker"), "{}", args[1]);
    let chats = fixture.ok(&["list", "--chats"]);
    let chat = &chats[0];
    assert_eq!(chat["name"], "worker chat", "{chats}");
    assert_eq!(chat["workdir"]["workspace"]["name"], "worker", "{chats}");
    let id = chat["id"].as_str().unwrap();
    assert_eq!(fs::read_to_string(home.join("chat-id")).unwrap().trim(), id);
    // The whole list names each space's kind and where it works: the
    // chat in the worktree it does not own.
    let listed = fixture.ok(&["list"]);
    let spaces = listed.as_array().unwrap();
    let kinds: Vec<_> = spaces.iter().map(|space| &space["kind"]).collect();
    assert_eq!(kinds, ["workspace", "chat"], "{listed}");
    for space in spaces {
        assert_eq!(space["workdir"]["path"], spaces[0]["path"], "{listed}");
    }

    // Workspace commands the chat runs act on the workspace, which its
    // scope reads but does not change.
    let scope = fs::read_to_string(home.join("chat-scope")).unwrap();
    let in_chat = |args: &[&str]| {
        fixture
            .command()
            .current_dir(&worktree)
            .env("SHOAL_SPACE", id)
            .env("SHOAL_SCOPE_TOKEN", scope.trim())
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    let status = in_chat(&["status"]);
    assert!(status.status.success(), "{}", printed(&status));
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["workspace"]["name"], "worker", "{status}");
    assert!(in_chat(&["conflicts"]).status.success());
    fs::write(
        worktree.join("tracked"),
        "changed by the workspace's agent\n",
    )
    .unwrap();
    let diff = in_chat(&["diff"]);
    assert!(diff.status.success(), "{}", printed(&diff));
    let diff = String::from_utf8_lossy(&diff.stdout);
    assert!(diff.contains("+changed by the workspace's agent"), "{diff}");
    let view = in_chat(&["view", "--space", &fixture.id("worker"), "--no-comments"]);
    assert!(view.status.success(), "{}", printed(&view));
    let change = in_chat(&["branch", "base", "clear"]);
    assert!(!change.status.success(), "{}", printed(&change));
    // A chat in a workspace takes its repository from it.
    let both = fixture.run(&[
        "start",
        "--mode",
        "chat",
        fixture.repo.to_str().unwrap(),
        "--workspace",
        "--branch",
        &fixture.id("worker"),
    ]);
    assert!(!both.status.success(), "{}", printed(&both));

    // Each status names the other.
    assert_eq!(
        fixture.ok(&["status", &fixture.id("worker")])["chats"][0]["name"],
        "worker chat"
    );

    // Its agent exited; `shoal restore` restores it in the worktree.
    assert_eq!(chats[0]["running"], false, "{chats}");
    fs::remove_file(home.join("chat-cwd")).unwrap();
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .args(["restore", &fixture.id("worker chat"), "--focus"]),
        "",
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(
        fs::read_to_string(home.join("chat-cwd")).unwrap().trim(),
        worktree.to_str().unwrap()
    );
    assert_eq!(fs::read_to_string(home.join("chat-id")).unwrap().trim(), id);

    let (no, prompt) = fixture.interactive(&["rm", "--wait", &fixture.id("worker")], "n\n");
    assert!(!no.status.success());
    assert!(prompt.contains("each back: worker chat"), "{prompt}");
    let removed = fixture.run(&["rm", "--wait", &fixture.id("worker"), "--yes"]);
    assert!(removed.status.success(), "{}", printed(&removed));
    assert_eq!(fixture.ok(&["list", "--chats"]), serde_json::json!([]));
}

#[test]
fn chat_starts_from_the_checkout_without_syncing() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    install_test_script(
        home.join("bin/claude"),
        "#!/bin/sh\ngit rev-parse HEAD > \"$HOME/chat-head\"\nprintf '%s\\0' \"$@\" > \"$HOME/chat-args\"\n",
    );
    let author = upstream_remote(&fixture);
    commit_file(
        &author,
        ".shoal.toml",
        "rules.chat.text = 'Upstream chat guidance'\n",
    );
    git(&author, &["push", "origin", "main"]);
    let chat = || {
        let (output, transcript) = fixture.terminal_command(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .env("ZMX_SESSION", "outer")
                .args(["start", "--mode", "chat", "--agent", "claude"]),
            "\n",
            true,
        );
        assert!(
            output.status.success(),
            "{}\n{transcript}",
            printed(&output)
        );
        let head = fs::read_to_string(home.join("chat-head")).unwrap();
        (
            head,
            format!("{}{transcript}", String::from_utf8_lossy(&output.stderr)),
        )
    };
    // An agent that cannot run is refused before the chat starts.
    let (output, transcript) = fixture.interactive_command(
        fixture.command().current_dir(&fixture.repo).args([
            "start",
            "--mode",
            "chat",
            "--agent",
            "happy-claude",
        ]),
        "",
    );
    assert!(!output.status.success(), "{transcript}");
    // The chat starts from the checkout as the sync left it, and the
    // instructions come from that checkout.
    fixture.ok(&["repo", "sync", fixture.repo.to_str().unwrap()]);
    let upstream = git(&author, &["rev-parse", "HEAD"]);
    let (head, output) = chat();
    assert_eq!(head, upstream);
    assert!(!output.contains("Syncing"), "{output}");
    let instructions = &chat_args(&fixture)[1];
    assert!(
        instructions.contains("Upstream chat guidance"),
        "{instructions}"
    );
    // A default branch no sync can fast-forward does not hold the chat back.
    commit_file(&fixture.repo, "local", "local\n");
    let local = git(&fixture.repo, &["rev-parse", "main"]);
    commit_file(&author, "remote", "remote\n");
    git(&author, &["push", "origin", "main"]);
    let (head, _) = chat();
    assert_eq!(head, local);
}

/// A fixture whose chat's focused Herdr tab has opened, its chat recorded
/// without a launch entry, since no limit applies, and the command that
/// runs the tab's own call.
fn chat_with_an_open_tab() -> (Fixture, Command) {
    let fixture = Fixture::with_config(Some(
        "terminal.focus = false\nmachine.max_heavy_agents = 'none'\n",
    ));
    install_fake_herdr(&fixture);
    install_recording_agent(&fixture, "codex");
    let repo = fixture.repo.to_str().unwrap();
    let start = ["start", "--mode", "chat", repo, "--agent", "codex"];
    let (caller, transcript) = herdr_call(&fixture, &start, "\n");
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    assert_eq!(
        fixture.ok(&["list", "--chats"]).as_array().unwrap().len(),
        1
    );
    let mut tab = herdr_worker_command(&fixture);
    tab.current_dir(&fixture.repo)
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p9");
    (fixture, tab)
}

/// A chat whose focused tab opens, but whose start there fails before its
/// agent runs, stays with its failed launch, as a workspace's would, for
/// `shoal restore` to start again.
#[test]
fn a_chat_whose_tab_fails_to_start_its_agent_stays_with_its_failed_launch() {
    let (fixture, mut tab) = chat_with_an_open_tab();
    fs::remove_file(fixture.root.path().join("bin/codex")).unwrap();

    let (output, transcript) = fixture.interactive_command(&mut tab, "");

    assert!(!output.status.success(), "{transcript}");
    assert_failed_launch_kept(&fixture, &transcript);
}

/// Check that `fixture`'s one chat stays with its failed launch in the
/// queue, which `context` explains.
fn assert_failed_launch_kept(fixture: &Fixture, context: &str) {
    let chats = fixture.ok(&["list", "--chats"]);
    let [chat] = chats.as_array().unwrap().as_slice() else {
        panic!("{chats} {context}");
    };
    let queue = fixture.ok(&["queue"]);
    let failed: Vec<_> = (queue.as_array().unwrap().iter())
        .filter(|entry| entry["state"] == "failed")
        .map(|entry| entry["space_id"].clone())
        .collect();
    assert_eq!(failed, [chat["id"].clone()], "{context}");
}

/// A chat whose tab's start fails before it takes the recorded chat, as one
/// without a terminal does, stays with its failed launch too.
#[test]
fn a_chat_whose_tab_fails_before_taking_it_stays_with_its_failed_launch() {
    let (fixture, mut tab) = chat_with_an_open_tab();

    let output = tab.stdin(Stdio::null()).output().unwrap();

    assert!(!output.status.success(), "{}", printed(&output));
    assert_failed_launch_kept(&fixture, &printed(&output).to_string());
}

#[test]
fn chat_opens_a_focused_herdr_tab_at_the_checkout() {
    let fixture = Fixture::with_config(Some("terminal.focus = false\n"));
    install_fake_herdr(&fixture);
    install_recording_agent(&fixture, "codex");
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let id = repository["id"].as_str().unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();

    let (caller, transcript) = herdr_call(
        &fixture,
        &[
            "start",
            "--mode",
            "chat",
            fixture.repo.to_str().unwrap(),
            "--agent",
            "codex",
            "--",
            "--search",
        ],
        "\n",
    );
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    let create = &herdr_calls(&fixture)[0];
    assert_eq!(create[..2], ["tab", "create"]);
    let option = |name: &str| {
        let at = create.iter().position(|arg| arg == name).unwrap();
        create[at + 1].clone()
    };
    assert_eq!(Path::new(&option("--cwd")), fixture.repo);
    assert_eq!(option("--label"), CHAT_NAME);
    assert!(create.iter().any(|arg| arg == "--focus"));
    // The start recorded the chat before the tab opened, passes the tab
    // that chat and records it for `attach -` once it shows the tab.
    let chats = fixture.ok(&["list", "--chats"]);
    let chat_id = chats[0]["id"].as_str().unwrap();
    let worker = herdr_worker(&fixture);
    let chat = worker.iter().position(|arg| arg == "start").unwrap();
    assert_eq!(
        worker[chat..],
        [
            "start",
            id,
            "--mode",
            "chat",
            "--as-chat",
            "--here",
            "--agent",
            "codex",
            "--name",
            CHAT_NAME,
            "--chat",
            chat_id,
            "--",
            "--search"
        ]
    );
    let history = fixture.root.path().join("state/recent-workspaces.json");
    let recorded: Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert_eq!(recorded["attach"], serde_json::json!([chat_id]));

    // The tab's own call runs the agent in its pane.
    let (output, transcript) = fixture.interactive_command(
        herdr_worker_command(&fixture)
            .current_dir(&fixture.repo)
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p9"),
        "",
    );
    assert!(output.status.success(), "{} {transcript}", printed(&output));
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("chat-cwd"))
            .unwrap()
            .trim(),
        checkout.to_str().unwrap()
    );
    let args = chat_args(&fixture);
    assert_eq!(args[0], "-c");
    assert!(
        args[1].starts_with("developer_instructions="),
        "{}",
        args[1]
    );
    // Its sandbox lets it reach the daemon, after its hooks.
    let sandbox = (args.iter())
        .position(|arg| arg == "sandbox_workspace_write.network_access=true")
        .unwrap();
    assert_eq!(args[sandbox - 1], "-c");
    // It runs a workspace agent's hooks without the subagent brief.
    let hooks = args[2..sandbox - 1].chunks(2).map(|flag| {
        assert_eq!(flag[0], "-c");
        flag[1].split_once('=').unwrap().0
    });
    assert_eq!(
        hooks.collect::<Vec<_>>(),
        [
            "hooks.PostToolUse",
            "hooks.SessionStart",
            "hooks.Stop",
            "hooks.SubagentStart",
            "hooks.SubagentStop",
            "hooks.UserPromptSubmit"
        ]
    );
    let start = args
        .iter()
        .find(|arg| arg.starts_with("hooks.SubagentStart="));
    assert!(!start.unwrap().contains("subagent-start"), "{start:?}");
    // Codex's default launcher appends its own options after forwarded ones.
    assert_eq!(after_temp_dirs(&args[sandbox + 1..])[0], "--search");
    let codex = fs::read_to_string(fixture.root.path().join(".codex/config.toml")).unwrap();
    assert!(
        codex.contains(checkout.to_str().unwrap()) && codex.contains("trusted"),
        "{codex}"
    );
    assert!(!fixture.root.path().join("zmx-calls").exists());
}

#[test]
fn chat_agents_without_an_instructions_template_get_them_as_their_first_message() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    let chat = |agent: &str| {
        install_recording_agent(&fixture, agent);
        let (output, transcript) = fixture.interactive_command(
            fixture.command().args([
                "start", "--mode", "chat", repo, "--agent", agent, "--here", "--", "--fast",
            ]),
            "\n",
        );
        assert!(
            output.status.success(),
            "{}\n{transcript}",
            printed(&output)
        );
        chat_args(&fixture)
    };
    // The message leads the arguments, and asks the agent to wait for the user.
    let args = chat("pi");
    assert_eq!(args.len(), 2, "{args:?}");
    assert!(args[0].contains(checkout.to_str().unwrap()), "{}", args[0]);
    assert_eq!(args[1], "--fast");
    // A launcher with `{prompt}` takes it there.
    assert_eq!(chat("opencode"), ["--prompt", &args[0], "--fast"]);
}

#[test]
fn a_chat_on_an_issue_is_assigned_the_issue() {
    let fixture = Fixture::new();
    forge_origin(&fixture);
    // The agents' directory is on the terminal's PATH.
    install_api_curl(&fixture.root.path().join("bin"));
    let url = "https://forge.example/team/project/issues/12";
    script_api(
        fixture.root.path(),
        "repos/team/project/issues/12",
        &serde_json::json!({"number": 12, "title": "Clarify retries", "state": "open",
            "body": "Body.", "labels": [], "html_url": url, "pull_request": null}),
    );
    // The chat's agent reads what its chat links.
    install_test_script(
        fixture.root.path().join("bin").join("pi"),
        format!(
            "#!/bin/sh\n'{}' --json view --space \"$SHOAL_SPACE\" --no-comments > \"$HOME/plan-view\"\n",
            support::shoal_binary()
        ),
    );
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .args([
                "start", "--mode", "plan", "--issue", url, "--agent", "pi", "--here",
            ])
            .current_dir(&fixture.repo),
        "",
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let viewed: Value =
        serde_json::from_str(&fs::read_to_string(fixture.root.path().join("plan-view")).unwrap())
            .unwrap();
    assert_eq!(viewed[0]["url"], url, "{viewed}");
}

#[test]
fn chat_picks_chat_agents_and_refuses_unsupported_launches() {
    let fixture = Fixture::new();
    install_fake_agents(&fixture, &["claude", "happy"]);
    // The background place runs the chat's shell here, whose login profile
    // would put the runner's own agents first on its PATH.
    install_test_script(fixture.root.path().join("bin/zmx"), terminal_zmx());
    let repo = fixture.repo.to_str().unwrap();
    // Without a default agent a picker offers the installed agents that can run.
    let (output, rows) = fixture.pick(&["start", repo, "--mode", "chat"], "0", "\n");
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        rows.lines()
            .map(|row| row.split('\t').nth(1).unwrap())
            .collect::<Vec<_>>(),
        ["claude"]
    );
    // An agent that cannot run a chat is refused before the mode picker.
    let args = ["start", "--mode", "chat", repo, "--agent", "happy-claude"];
    let (output, rows) = fixture.pick(&args, "chat", "");
    assert!(!output.status.success(), "{}", printed(&output));
    assert!(rows.is_empty(), "a picker opened: {rows}");
    // Without a terminal, the daemon starts the chat in a background place.
    let output = fixture.run(&["start", "--mode", "chat", repo, "--agent", "claude"]);
    assert!(output.status.success(), "{}", printed(&output));
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(chats[0]["agent"], "claude", "{chats}");
    // The checkout has no value for a workspace's placeholders.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[agents.claude]\ncommand = ['claude', '--name', '{workspace}']\n",
    )
    .unwrap();
    let (output, transcript) = fixture.interactive_command(
        fixture.command().args([
            "start", repo, "--agent", "claude", "--here", "--mode", "chat",
        ]),
        "\n",
    );
    assert!(!output.status.success());
    assert!(transcript.contains("{workspace}"), "{transcript}");
}

#[test]
fn attach_returns_to_chats_by_name() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let other = repository_with(home, "other", &[("tracked", "committed\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    // Sessions are found where the chat was recorded, whatever their name.
    let (_agents, ids) = running_chats(
        &fixture,
        &[
            (&fixture.repo, "talk", Some("second")),
            (&fixture.repo, "plan-7", Some("first")),
            (&other, "quiet", None),
        ],
    );
    let (talk, plan) = (&ids[0], &ids[1]);
    let attached = || {
        zmx_calls(&fixture)
            .into_iter()
            .rfind(|call| call[0] == "attach")
            .unwrap()
    };

    let (output, transcript) = fixture.interactive(&["attach", &fixture.id("talk")], "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(attached(), ["attach", "second"]);
    let (output, transcript) = fixture.interactive(&["attach", &fixture.id("plan-7")], "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(attached(), ["attach", "first"]);

    // In a checkout with two chats running, the picker offers the chats with
    // sessions by name.
    let (output, rows) = fixture.pick_in(&fixture.repo, &["attach"], talk, "");
    assert!(output.status.success(), "{}", printed(&output));
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (id, name) in [(&talk, "talk"), (&plan, "plan-7")] {
        assert!(
            rows.iter()
                .any(|row| row.starts_with(id.as_str()) && row.contains(name)),
            "{rows:?}"
        );
    }
    assert_eq!(attached(), ["attach", "second"]);

    // A chat without a session says where it runs instead, whether named or
    // taken as its checkout's only chat.
    let mut named = fixture.command();
    named.args(["attach", &fixture.id("quiet")]);
    let mut current = fixture.command();
    current.current_dir(&other).arg("attach");
    for command in [&mut named, &mut current] {
        let (output, transcript) = fixture.interactive_command(command, "");
        assert!(!output.status.success(), "{transcript}");
    }
}

#[test]
fn chat_rename_renames_the_named_current_or_picked_chat() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let other = repository_with(home, "other", &[("tracked", "committed\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let (_agents, ids) = running_chats(
        &fixture,
        &[
            (&fixture.repo, "talk", Some("talk")),
            (&fixture.repo, "repo-plan-7", None),
            (&other, "quiet", None),
        ],
    );
    let (talk, plan) = (ids[0].clone(), ids[1].clone());
    let names = || -> Vec<String> {
        let chats = fixture.ok(&["list", "--chats"]);
        chats
            .as_array()
            .unwrap()
            .iter()
            .map(|chat| chat["name"].as_str().unwrap().to_owned())
            .collect()
    };

    // By ID; the zmx session keeps its name, and attach finds the chat.
    let renamed = fixture.ok(&["rename", &fixture.id("talk"), "topic"]);
    assert_eq!(renamed["id"], talk.as_str());
    assert_eq!(renamed["name"], "topic");
    let (output, transcript) = fixture.interactive(&["attach", &fixture.id("topic")], "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );

    // The chat this runs in, as its agent would.
    let output = fixture
        .command()
        .env("SHOAL_SPACE", &plan)
        .args(["rename", "repo-plan-8"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));

    // The checkout's only chat, else one from a picker.
    let output = fixture
        .command()
        .current_dir(&other)
        .args(["rename", "calm"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    // A name is one line.
    let output = fixture
        .command()
        .current_dir(&fixture.repo)
        .args(["rename", &fixture.id("calm"), "bad\nname"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let (output, rows) = fixture.pick_in(&fixture.repo, &["rename", "picked"], &talk, "");
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(rows.lines().count(), 3, "{rows}");
    assert_eq!(names(), ["picked", "repo-plan-8", "calm"]);
}

#[test]
fn a_name_request_reaches_the_chats_agent() {
    let fixture = Fixture::new();
    let (_agent, chat) = fixture.run_chat("talk", &[]);
    let output = fixture.run(&["rename", "--request", &chat]);
    assert!(output.status.success(), "{}", printed(&output));
    // The chat's agent reads it as a message from the user, about no
    // workspace.
    let output = fixture
        .command()
        .env("SHOAL_SPACE", &chat)
        .args(["--json", "messages"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    let messages: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(messages.as_array().map(Vec::len), Some(1), "{messages}");
    // Workspace processes do not reach chats.
    fixture.add("worker");
    let output = scoped_command(&fixture, "worker", &["rename", "--request", "talk"]);
    assert!(!output.status.success(), "{}", printed(&output));
}

#[test]
fn a_restored_chat_continues_its_session_with_its_messages_waiting() {
    let fixture = chat_session_fixture();
    let home = fixture.root.path().to_owned();
    save_running_chat(&fixture, |fixture| {
        let output = fixture.run(&["stop", &fixture.id("talk")]);
        assert!(output.status.success(), "{}", printed(&output));
    });
    let talk = fixture.id("talk");
    let output = fixture.run(&[
        "messages",
        "send",
        "--space",
        &talk,
        "What blocked you cleared",
    ]);
    assert!(output.status.success(), "{}", printed(&output));

    let (output, transcript) =
        fixture.interactive_command(fixture.command().args(["restore", &talk, "--focus"]), "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let args = fs::read_to_string(home.join("chat-args")).unwrap();
    assert!(args.contains("--resume\nchat-session\n"), "{args}");
    // They wait for its hook or its wrapper's wake.
    let messages = chat_messages(&fixture, &talk);
    assert!(messages.contains("What blocked you cleared"), "{messages}");
}

/// The messages waiting for the agent of the chat `id`, as it reads them.
fn chat_messages(fixture: &Fixture, id: &str) -> String {
    let output = fixture
        .command()
        .env("SHOAL_SPACE", id)
        .args(["--json", "messages"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_removed_chat_restores_under_its_id_from_the_removed_chats() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    fixture.ok(&["rm", "--wait", &chat, "--yes"]);
    agent.wait().unwrap();
    assert_eq!(fixture.ok(&["list", "--chats"]), serde_json::json!([]));

    // Without a terminal, restore lists it among the removed spaces.
    let listed = fixture.ok(&["restore"]);
    let removed = listed["removed"].as_array().unwrap();
    assert_eq!(removed.len(), 1, "{listed}");
    assert_eq!(removed[0]["workspace_id"], chat.as_str(), "{listed}");
    assert!(removed[0]["chat"].is_object(), "{listed}");

    let restored = fixture.ok(&["restore", &chat, "--no-agents"]);
    let reopened = &restored["reopened"][0];
    assert_eq!(reopened["space"]["kind"], "chat", "{restored}");
    assert_eq!(reopened["space"]["id"], chat.as_str(), "{restored}");
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(chats[0]["id"], chat.as_str(), "{chats}");
    assert_eq!(chats[0]["name"], "planning", "{chats}");
    assert_eq!(fixture.ok(&["restore"])["removed"], serde_json::json!([]));
}

/// A chat's failed launch restores only with its agent: `--no-agents`
/// leaves it failed, and `shoal restore` queues it again and reports it.
#[test]
fn a_chats_failed_launch_restores_only_with_its_agent() {
    let (fixture, mut tab) = chat_with_an_open_tab();
    let output = tab.stdin(Stdio::null()).output().unwrap();
    assert!(!output.status.success(), "{}", printed(&output));
    let chats = fixture.ok(&["list", "--chats"]);
    let chat = chats[0]["id"].as_str().unwrap().to_owned();

    fixture
        .command()
        .args(["restore", &chat, "--no-agents"])
        .output()
        .unwrap();
    assert_failed_launch_kept(&fixture, "after --no-agents");

    let restored = fixture.ok(&["restore", &chat]);
    let queued: Vec<_> = (restored["queued"].as_array().unwrap().iter())
        .map(|entry| entry["space_id"].as_str().unwrap())
        .collect();
    assert_eq!(queued, [chat.as_str()], "{restored}");
}

/// A failed chat launch the restore picker chooses queues again, as one
/// the command line names does.
#[test]
fn a_failed_chat_launch_picked_to_restore_queues_again() {
    let (fixture, mut tab) = chat_with_an_open_tab();
    let output = tab.stdin(Stdio::null()).output().unwrap();
    assert!(!output.status.success(), "{}", printed(&output));

    // The picker takes the first restorable space, the failed chat.
    let fzf = fixture.root.path().join("bin/fzf");
    install_test_script(fzf, "#!/bin/sh\nhead -n 1\n");
    let mut restore = fixture.command();
    restore.arg("restore");
    let (output, transcript) = fixture.interactive_command(&mut restore, "");

    assert!(output.status.success(), "{} {transcript}", printed(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Queued 1 agent"), "{}", printed(&output));
}
