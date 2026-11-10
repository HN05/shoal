//! Chats: their sessions and places, renames, the spaces below them and
//! their agents' scope.
use super::forge::{forge_origin, github_issue, install_api_curl, script_api};
use super::herdr::{
    herdr_call, herdr_calls, herdr_worker, herdr_worker_command, install_fake_herdr,
};
use super::start::guidance_end;
use super::zmx::zmx_calls;
use super::*;

/// A fixture whose Claude chat agent reports its session as Claude Code's
/// hook does, writes to its temporary directory, then talks until a signal
/// ends it; resumed, it only records how it ran.
pub(crate) fn chat_session_fixture() -> Fixture {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './chat-agent.sh']\n",
    ));
    fs::write(
        fixture.repo.join("chat-agent.sh"),
        format!(
            "printf '%s\\n' \"$@\" > \"$HOME/chat-args\"\n\
             echo \"$SHOAL_CHAT\" > \"$HOME/chat-id\"\n\
             echo \"$TMPDIR\" > \"$HOME/chat-tmpdir\"\n\
             case \" $* \" in *' --resume '*) exit 0;; esac\n\
             printf '{{\"session_id\":\"chat-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n\
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
                    "start", "--chat", "--here", "--agent", "claude", "--name", "talk",
                ]),
                "",
            )
        });
        wait_until("chat started", || home.join("chat-started").exists());
        let chats = fixture.ok(&["work", "--chats"]);
        stop(fixture);
        let (output, transcript) = chat.join().unwrap();
        assert!(output.status.success(), "{output:?}\n{transcript}");
        chats[0].clone()
    });
    assert_eq!(chat["name"], "talk", "{chat}");
    chat
}

#[test]
fn a_saved_chats_open_question_stays_in_the_inbox() {
    let mut fixture = chat_session_fixture();
    save_running_chat(&fixture, |fixture| {
        let chat = fixture.ok(&["work", "--chats"])[0]["id"].clone();
        let output = fixture
            .command()
            .current_dir(&fixture.repo)
            .env("SHOAL_CHAT", chat.as_str().unwrap())
            .args(["--json", "questions", "ask", "Which database?"])
            .args(["--recommend", "SQLite"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let output = fixture.run(&["daemon", "stop"]);
        assert!(output.status.success(), "{output:?}");
    });
    assert!(fixture.daemon.child.wait().unwrap().success());
    fixture.restart();
    let inbox = fixture.ok(&["inbox"]);
    assert_eq!(inbox[0]["question"]["space"], "talk", "{inbox}");
    let listed = fixture.ok(&["questions", "--space", "talk"]);
    assert_eq!(listed["questions"][0]["space"], "talk", "{listed}");
}

#[test]
fn a_chat_daemon_stop_saved_resumes_under_its_name_and_session() {
    let mut fixture = chat_session_fixture();
    let home = fixture.root.path().to_owned();
    // `shoal daemon stop` saves the chat for `shoal resume`.
    let chat = save_running_chat(&fixture, |fixture| {
        let output = fixture.run(&["daemon", "stop"]);
        assert!(output.status.success(), "{output:?}");
    });
    assert!(fixture.daemon.child.wait().unwrap().success());
    fixture.restart();
    let id = chat["id"].as_str().unwrap();
    let tmp_dir = PathBuf::from(fs::read_to_string(home.join("chat-tmpdir")).unwrap().trim());

    // A workspace process neither restores nor discards it, even with its
    // scope token cleared.
    fixture.add("worker");
    let output = fixture
        .command()
        .args(["run", "--workspace", "worker", "--", "sh", "-c"])
        .arg(r#"env -u SHOAL_SCOPE_TOKEN "$0" resume talk --discard; exit $?"#)
        .arg(support::shoal_binary())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    // A restore that fails where nobody watches, before it takes the chat
    // over, tells the user.
    let output = fixture.run(&[
        "start",
        "--chat",
        "no-such-repository",
        "--here",
        "--name",
        "talk",
        "--agent",
        "claude",
        "--restore",
        id,
    ]);
    assert!(!output.status.success(), "{output:?}");
    let alerts = fixture.ok(&["alerts", "--all"]);
    assert!(
        alerts
            .as_array()
            .unwrap()
            .iter()
            .any(|alert| alert["kind"] == "chat_restore_failed"),
        "{alerts}"
    );

    let saved = fixture.ok(&["work", "--chats"]);
    assert_eq!(saved[0]["id"], id, "{saved}");
    assert_eq!(saved[0]["name"], "talk", "{saved}");
    assert_eq!(saved[0]["restore"], "resume", "{saved}");
    assert!(tmp_dir.join("notes").exists());

    // A restore whose agent cannot run once it took the chat over leaves the
    // chat saved, with its files, and tells the user.
    fs::write(fixture.repo.join("agent.txt"), "not a program\n").unwrap();
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[agents.claude]\ncommand = ['./agent.txt']\n",
    )
    .unwrap();
    let (output, transcript) =
        fixture.interactive_command(fixture.command().args(["resume", "talk", "--focus"]), "");
    assert!(!output.status.success(), "{output:?}\n{transcript}");
    let saved = fixture.ok(&["work", "--chats"]);
    assert_eq!(saved[0]["restore"], "resume", "{saved}");
    assert!(tmp_dir.join("notes").exists());
    let failures = fixture
        .ok(&["alerts", "--all"])
        .as_array()
        .unwrap()
        .iter()
        .filter(|alert| alert["kind"] == "chat_restore_failed")
        .count();
    assert_eq!(failures, 2);
    fs::remove_file(fixture.repo.join(".shoal.toml")).unwrap();
    fs::remove_file(fixture.repo.join("agent.txt")).unwrap();

    let (output, transcript) =
        fixture.interactive_command(fixture.command().args(["resume", "talk", "--focus"]), "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let args = fs::read_to_string(home.join("chat-args")).unwrap();
    assert!(args.contains("--resume\nchat-session\n"), "{args}");
    assert_eq!(fs::read_to_string(home.join("chat-id")).unwrap().trim(), id);
    assert_eq!(
        fs::read_to_string(home.join("chat-tmpdir")).unwrap().trim(),
        tmp_dir.to_str().unwrap()
    );
    // The resumed agent exited, which stops the chat again; listing chats
    // finds that.
    assert_eq!(fixture.ok(&["work", "--chats"])[0]["restore"], "resume");
    assert!(tmp_dir.exists());
    assert_eq!(
        chat_kinds(&fixture),
        [
            "chat_started",
            "chat_saved",
            // The restore whose agent could not run.
            "chat_restored",
            "chat_saved",
            "chat_restored",
            "chat_saved"
        ]
    );
}

#[test]
fn a_resume_takes_saved_chats_and_workspaces_together() {
    let mut fixture = chat_session_fixture();
    let home = fixture.root.path().to_owned();
    let save_chat = |fixture: &mut Fixture| {
        let _ = fs::remove_file(home.join("chat-started"));
        let chat = save_running_chat(fixture, |fixture| {
            let output = fixture.run(&["daemon", "stop"]);
            assert!(output.status.success(), "{output:?}");
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
            r#"{"agent":"claude"}"#,
        )
        .unwrap();
    };
    let discarded_both = |discarded: &Value| {
        assert_eq!(
            discarded["discarded"],
            serde_json::json!(["worker-agent"]),
            "{discarded}"
        );
        assert_eq!(discarded["chats"][0]["name"], "talk", "{discarded}");
    };
    save_chat(&mut fixture);
    let worker = fixture.add("worker");
    stop_agent(&fixture, &worker);
    // A list naming a chat and a workspace acts on both.
    discarded_both(&fixture.ok(&["resume", "talk", "worker", "--discard"]));
    assert_eq!(fixture.ok(&["work", "--chats"]), Value::Array(vec![]));
    // `--all` takes the saved chats too.
    save_chat(&mut fixture);
    stop_agent(&fixture, &worker);
    discarded_both(&fixture.ok(&["resume", "--all", "--discard"]));
    assert_eq!(fixture.ok(&["work", "--chats"]), Value::Array(vec![]));
    // A run that restores a chat lists the only agent for its own terminal
    // instead of restoring it here in place of the run's report. The chat
    // opens in a zmx session that gives it a terminal.
    save_chat(&mut fixture);
    stop_agent(&fixture, &worker);
    install_test_script(home.join("bin").join("zmx"), terminal_zmx());
    let saves = |fixture: &Fixture| {
        let kinds = chat_kinds(fixture);
        kinds.iter().filter(|kind| *kind == "chat_saved").count()
    };
    let saved = saves(&fixture);
    let (output, transcript) =
        fixture.interactive_command(fixture.command().args(["resume", "--all", "--now"]), "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--execution worker-agent"), "{output:?}");
    // The restored agent only records how it ran, which stops the chat.
    wait_until("the restored chat to stop", || saves(&fixture) > saved);
    fixture.ok(&["resume", "talk", "worker", "--discard"]);
    // The picker offers the saved chats after the workspaces with stopped
    // work.
    let chat = save_chat(&mut fixture);
    stop_agent(&fixture, &worker);
    let both = format!("{chat} {}", worker["id"].as_str().unwrap());
    let (output, rows) = fixture.pick(&["resume", "--discard"], &both, "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].contains("worker"), "{rows:?}");
    assert!(rows[1].contains("talk"), "{rows:?}");
    assert_eq!(fixture.ok(&["work", "--chats"]), Value::Array(vec![]));
}

#[test]
fn a_chat_a_signal_saved_comes_back_on_its_own() {
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

    // The resumed agent records its arguments, then its chat, then exits.
    wait_until("chat restored", || {
        fs::read_to_string(home.join("chat-args"))
            .is_ok_and(|args| args.contains("--resume\nchat-session\n"))
    });
    // The fake opened it, not a zmx the runner may have installed.
    let calls = fs::read_to_string(home.join("zmx-calls")).unwrap();
    assert!(calls.contains("\"run\""), "{calls}");
    wait_until("chat-id rewritten", || {
        fs::read_to_string(home.join("chat-id"))
            .is_ok_and(|id| id.trim() == chat["id"].as_str().unwrap())
    });
    wait_until("restored chat stopped", || {
        chat_kinds(&fixture) == ["chat_started", "chat_saved", "chat_restored", "chat_saved"]
    });
    assert_eq!(fixture.ok(&["work", "--chats"])[0]["restore"], "resume");
}

/// A [`PASSTHROUGH_ZMX`] whose sessions run their command on a terminal of
/// their own, as a chat needs. It waits for the command rather than for the
/// terminal to close, which `pty.spawn` waits for and macOS may never
/// report, leaving the session behind.
pub(crate) fn terminal_zmx() -> String {
    // The command runs inside the worker's `try`, which removes its
    // running marker however it ends.
    let zmx = PASSTHROUGH_ZMX.replace(
        "        sys.exit(subprocess.run(['sh', '-c', args[1]]).returncode)",
        "        import pty, select\n\
         \x20       pid, terminal = pty.fork()\n\
         \x20       if pid == 0:\n\
         \x20           os.execvp('sh', ['sh', '-c', args[1]])\n\
         \x20       while not (status := os.waitpid(pid, os.WNOHANG))[0]:\n\
         \x20           if select.select([terminal], [], [], 0.1)[0]:\n\
         \x20               try:\n\
         \x20                   os.read(terminal, 4096)\n\
         \x20               except OSError:\n\
         \x20                   pass\n\
         \x20       sys.exit(os.waitstatus_to_exitcode(status[1]))",
    );
    assert_ne!(zmx, PASSTHROUGH_ZMX);
    zmx
}

/// The kinds of the journal's chat events, in order.
fn chat_kinds(fixture: &Fixture) -> Vec<String> {
    journal(fixture)
        .into_iter()
        .filter(|event| event["type"] == "chat")
        .map(|event| event["kind"].as_str().unwrap().to_owned())
        .collect()
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
        .args(["--json", "start", fixture.repo.to_str().unwrap(), "opened"])
        .env("SHOAL_CHAT", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let opened: Value = serde_json::from_slice(&output.stdout).unwrap();
    // A workspace that a chat below the chat opened is below it too.
    let mut nested_agent = support::isolated(fixture.root.path(), "sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let repository = fixture.ok(&["repo", "ls"])[0]["id"].clone();
    let response = fixture.request(serde_json::json!({"start_chat":{
        "repository": repository, "name": "nested", "agent": "claude",
        "pid": nested_agent.id(), "parent": chat
    }}));
    let nested = response["data"]["id"].as_str().unwrap().to_owned();
    let output = fixture
        .command()
        .args(["--json", "start", fixture.repo.to_str().unwrap(), "deeper"])
        .env("SHOAL_CHAT", &nested)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let deeper: Value = serde_json::from_slice(&output.stdout).unwrap();
    let other = fixture.add("other");
    let rename_in = |directory: &Value, name: &str| {
        let output = fixture
            .command()
            .current_dir(directory.as_str().unwrap())
            .env("SHOAL_CHAT", &chat)
            .args(["--json", "work", "rename", name])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
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
        fixture.ok(&["work", "status", "other"])["workspace"]["title"],
        Value::Null
    );
    for agent in [&mut agent, &mut nested_agent] {
        agent.kill().unwrap();
        agent.wait().unwrap();
    }
}

/// Start a chat named `name` in the fixture's repository, whose agent the
/// returned sleep stands in for, and return it with the chat's ID.
pub(crate) fn start_test_chat(fixture: &Fixture, name: &str) -> (std::process::Child, String) {
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let agent = support::isolated(fixture.root.path(), "sleep")
        .arg("600")
        .current_dir(fixture.root.path())
        .spawn()
        .unwrap();
    let response = fixture.request(serde_json::json!({"start_chat":{
        "repository": repository["id"], "name": name, "agent": "claude",
        "pid": agent.id()
    }}));
    assert_eq!(response["type"], "chat", "{response}");
    let chat = response["data"]["id"].as_str().unwrap().to_owned();
    (agent, chat)
}

/// Status names a chat in the namespace it shares with workspaces and
/// leaves out what needs a worktree; workspace processes see neither the
/// chat nor another workspace.
#[test]
fn status_shows_a_chat_without_the_parts_a_worktree_has() {
    let fixture = Fixture::new();
    let own = fixture.add("own");
    let (mut agent, chat) = start_test_chat(&fixture, "planning");

    let status = fixture.ok(&["work", "status", "planning"]);
    assert_eq!(status["kind"], "chat", "{status}");
    assert_eq!(status["chat"]["id"], chat.as_str(), "{status}");
    assert_eq!(status["sessions"], serde_json::json!([]), "{status}");
    for worktree in ["workspace", "executions", "ports", "diff", "cleanup_reason"] {
        assert_eq!(status.get(worktree), None, "{worktree}: {status}");
    }

    let status = fixture.ok(&["work", "status", "own"]);
    assert_eq!(status["kind"], "workspace", "{status}");
    assert_eq!(status["workspace"]["id"], own["id"], "{status}");
    assert_eq!(status["setup_finished"], true, "{status}");

    fixture.add("other");
    for other in ["planning", "other"] {
        let output = fixture.run(&[
            "run",
            "--workspace",
            "own",
            "--",
            support::shoal_binary(),
            "work",
            "status",
            other,
        ]);
        assert!(!output.status.success(), "{other}: {output:?}");
    }
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn the_inbox_acts_on_a_chats_ready_row_as_a_chat() {
    let fixture = Fixture::new();
    let (mut agent, _) = start_test_chat(&fixture, "planning");
    fixture.ok(&["ready", "--space", "planning"]);
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
    // The branch and demo options are a workspace's.
    for flag in ["--integrated", "--keep-demos"] {
        let output = fixture.run(&["done", "planning", flag]);
        assert!(!output.status.success(), "{flag}: {output:?}");
    }
    // Done again at the same HEAD with nothing new changes nothing.
    fixture.ok(&["done", "planning"]);
    let output = fixture.run(&["done", "planning"]);
    assert!(!output.status.success(), "{output:?}");
    let completion = fixture.ok(&["done", "planning", "--attach", report.to_str().unwrap()]);
    assert_eq!(completion["cleanup"], false, "{completion}");
    assert_eq!(
        completion["results"][0]["name"], "report.md",
        "{completion}"
    );
    // The chat keeps running, finished, and the user hears of it.
    let finished = fixture.ok(&["work", "--chats", "--status", "finished"]);
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
    let files = fixture.ok(&["files", "planning"]);
    assert_eq!(files[0]["name"], "report.md", "{files}");
    assert_eq!(files[0]["attached_by"]["kind"], "user", "{files}");
    let notes = fixture.root.path().join("notes.md");
    fs::write(&notes, "notes\n").unwrap();
    let output = fixture
        .command()
        .args(["--json", "done", "--attach", notes.to_str().unwrap()])
        .env("SHOAL_CHAT", &chat)
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let files = fixture.ok(&["files", "planning"]);
    assert_eq!(files[1]["name"], "notes.md", "{files}");
    assert_eq!(files[1]["attached_by"]["kind"], "chat", "{files}");
    let completion: Value = serde_json::from_slice(&output.stdout).unwrap();
    let withdrawn = fixture.ok(&["done", "--undo", "planning"]);
    assert_eq!(withdrawn["withdrawn"]["results"], completion["results"]);
    let finished = fixture.ok(&["work", "--chats", "--status", "finished"]);
    assert!(!finished.to_string().contains("planning"), "{finished}");
    let output = fixture.run(&["done", "--undo", "planning"]);
    assert!(!output.status.success(), "{output:?}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn completion_cleanup_removes_a_done_chat_with_the_workspaces_below_it() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let output = fixture
        .command()
        .args(["--json", "start", fixture.repo.to_str().unwrap(), "opened"])
        .env("SHOAL_CHAT", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let completion = fixture.ok(&["done", "planning"]);
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
    assert_eq!(fixture.ok(&["work", "--chats"]), serde_json::json!([]));
    // The workspace below it went first, and its agent stopped.
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
    assert!(!agent.wait().unwrap().success());
}

/// A fake agent that records its directory and NUL-separated arguments.
pub(crate) fn install_recording_agent(fixture: &Fixture, agent: &str) {
    install_test_script(
        fixture.root.path().join("bin").join(agent),
        "#!/bin/sh\npwd > \"$HOME/chat-cwd\"\nprintf '%s\\0' \"$@\" > \"$HOME/chat-args\"\necho \"${ZMX_SESSION:-none}\" >> \"$HOME/chat-sessions\"\necho \"$SHOAL_CHAT\" > \"$HOME/chat-id\"\necho \"$SHOAL_SCOPE_TOKEN\" > \"$HOME/chat-scope\"\necho \"$SHOAL_OUTPUT\" > \"$HOME/chat-output\"\necho \"$SHOAL_WORKSPACE_ID$SHOAL_WORKSPACE\" > \"$HOME/chat-workspace\"\n",
    );
}

/// The default name of a chat about the fixture's repository.
pub(crate) const CHAT_NAME: &str = "repo-with---quotes----literal-chat";

/// Every retained journal event.
pub(crate) fn journal(fixture: &Fixture) -> Vec<Value> {
    let output = fixture
        .command()
        .args(["--json", "internal", "events"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
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
        "--chat",
        fixture.repo.to_str().unwrap(),
        "--agent",
        "claude",
        "--",
        "--model",
        "fast",
    ]));
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    assert_eq!(calls[0], ["list"]);
    // The session is named after the chat, cut to fit zmx's socket path.
    let session = &CHAT_NAME[..32];
    assert_eq!(
        calls[1][..4],
        ["attach", "--labels", "shoal.created=1", session]
    );
    // The session's Shoal records the chat under the name chosen here,
    // labels its session with it and becomes its agent.
    let chat = calls[1].iter().position(|arg| arg == "start").unwrap();
    assert_eq!(calls[1][chat - 1], "--text");
    assert_eq!(
        calls[1][chat..],
        [
            "start", id, "--mode", "chat", "--chat", "--here", "--agent", "claude", "--name",
            CHAT_NAME, "--", "--model", "fast"
        ]
    );
    let started = journal(&fixture)
        .into_iter()
        .find(|event| event["kind"] == "chat_started")
        .unwrap();
    assert_eq!(started["type"], "chat");
    assert_eq!(started["chat"]["repository_id"], id);
    assert_eq!(started["chat"]["agent"], "claude");
    assert_eq!(started["chat"]["name"], CHAT_NAME);
    let chat_id = started["chat"]["id"].as_str().unwrap();
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
        started["chat"]["id"].as_str().unwrap()
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
    // Chats are not workspace executions.
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));

    // Inside a session the agent replaces Shoal in place, labelling that
    // session, and a later chat starts another one instead of reusing it.
    let before = zmx_calls(&fixture).len();
    let (output, transcript) = launch(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .args([
                "start",
                "--chat",
                "--agent",
                "claude",
                "--name",
                "outer-chat",
            ]),
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    assert_eq!(calls.len(), before + 2);
    assert_eq!(calls[before], ["list"]);
    assert_eq!(calls[before + 1][..2], ["set", "outer"]);
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
                "start", "--chat", "--here", "--name", "new-chat", "--agent", "claude",
            ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let sessions = fs::read_to_string(home.join("zmx-sessions"))?;
    assert!(sessions.contains("shoal.chat="), "{sessions}");
    assert!(sessions.contains("user.label=keep"), "{sessions}");
    assert!(!sessions.contains("shoal.workspace="), "{sessions}");
    assert!(!sessions.contains("shoal.demo="), "{sessions}");
    fixture.ok(&["rm", "old-owner", "--yes"]);
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
            "#!/bin/sh\nshoal='{}'\n\"$shoal\" --json start \"$PWD\" chat-worker </dev/null > \"$HOME/chat-add\"\n\"$shoal\" --text work > \"$HOME/chat-work\"\n\"$shoal\" --json work --chats > \"$HOME/chat-chats\"\nprintf '%s' \"$TMPDIR\" > \"$HOME/chat-tmpdir\"\n",
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
            .args(["start", "--chat", "--agent", "claude"]),
        "\n",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let started = journal(&fixture)
        .into_iter()
        .find(|event| event["kind"] == "chat_started")
        .unwrap();
    let chat = &started["chat"]["id"];
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

    // While it runs, `work` lists the chat with its workspaces under it, and
    // the zmx session it runs in.
    let chats: Value =
        serde_json::from_str(&fs::read_to_string(home.join("chat-chats")).unwrap()).unwrap();
    let mut listed = started["chat"].clone();
    listed["origin"] = serde_json::json!({"host": "zmx", "place": "outer"});
    // The agent ran with its own temporary directory, which the listing names.
    let tmp_dir = fs::read_to_string(home.join("chat-tmpdir")).unwrap();
    assert!(
        tmp_dir.ends_with(&format!("/.tmp/{}", chat.as_str().unwrap())),
        "{tmp_dir}"
    );
    listed["tmp_dir"] = tmp_dir.into();
    listed["parent"] = Value::Null;
    listed["children"] = serde_json::json!([{
        "id": added["id"], "name": "chat-worker", "kind": "workspace", "branch": added["branch"],
    }]);
    assert_eq!(chats, serde_json::json!([listed]));
    // `work` shows the workspace under its chat.
    let rows = fs::read_to_string(home.join("chat-work")).unwrap();
    let rows: Vec<&str> = rows.lines().collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].contains(CHAT_NAME), "{rows:?}");
    assert!(rows[1].contains("└ chat-worker"), "{rows:?}");

    // Once its agent exits the chat stops, keeping its workspace.
    let chats = fixture.ok(&["work", "--chats"]);
    assert_eq!(chats[0]["restore"], "resume", "{chats}");
    assert_eq!(fixture.ok(&["work"])[0]["parent"]["id"], chats[0]["id"]);
    let stopped = journal(&fixture)
        .into_iter()
        .find(|event| event["kind"] == "chat_saved")
        .unwrap();
    assert_eq!(stopped["chat"]["id"], started["chat"]["id"]);

    // Opening it outside a chat gives it back to the user, so removing the
    // chat leaves it.
    let reopened = fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "chat-worker",
    ]);
    assert_eq!(reopened["parent"], Value::Null);
    let removed = fixture.ok(&["rm", CHAT_NAME]);
    assert_eq!(removed["chat"]["id"], *chat, "{removed}");
    assert!(removed["children"].is_null(), "{removed}");
    assert_eq!(fixture.ok(&["work"])[0]["name"], "chat-worker");
}

#[test]
fn removing_a_chat_removes_the_workspaces_below_it_and_stops_its_agent() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    // The chat's agent opens a workspace, and a child is split from it.
    let output = fixture
        .command()
        .args(["--json", "start", fixture.repo.to_str().unwrap(), "opened"])
        .env("SHOAL_CHAT", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fixture.ok(&["split", "below", "--workspace", "opened"]);
    // Another chat runs in the opened workspace's worktree.
    let mut resident = support::isolated(fixture.root.path(), "sleep")
        .arg("600")
        .current_dir(fixture.root.path())
        .spawn()
        .unwrap();
    let repository = fixture.ok(&["repo", "ls"])[0]["id"].clone();
    let response = fixture.request(serde_json::json!({"start_chat":{
        "repository": repository, "workspace": "opened", "name": "reading",
        "agent": "claude", "pid": resident.id()
    }}));
    assert_eq!(response["type"], "chat", "{response}");

    // Removing the chat asks first, naming what it takes.
    let (output, prompt) = fixture.interactive(&["rm", "planning"], "n\n");
    assert!(!output.status.success(), "{prompt}");
    assert!(prompt.contains("opened, reading, below"), "{prompt}");

    // Naming the resident chat too removes it once, with the chat above it.
    let removed = fixture.ok(&["rm", "planning", "reading", "--yes"]);
    assert_eq!(removed["failed"], serde_json::json!([]), "{removed}");
    let chats = removed["chats"].as_array().unwrap();
    assert_eq!(chats.len(), 1, "{removed}");
    assert_eq!(chats[0]["chat"]["id"], chat.as_str(), "{removed}");
    assert_eq!(
        chats[0]["children"],
        serde_json::json!(["reading", "below", "opened"])
    );
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
    assert_eq!(fixture.ok(&["work", "--chats"]), serde_json::json!([]));
    assert!(!agent.wait().unwrap().success());
    assert!(!resident.wait().unwrap().success());
}

#[test]
fn a_workspace_a_chat_opens_is_below_it_and_in_its_repository() {
    let fixture = Fixture::new();
    let (mut agent, chat) = start_test_chat(&fixture, "planning");
    let add = |repository: &Path, name: &str| {
        fixture
            .command()
            .args(["--json", "start", repository.to_str().unwrap(), name])
            .env("SHOAL_CHAT", &chat)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let output = add(&fixture.repo, "opened");
    assert!(output.status.success(), "{output:?}");
    fixture.ok(&["split", "below", "--workspace", "opened"]);

    // One parent link names the chat or the workspace above, of either kind.
    let opened = fixture.ok(&["work", "status", "opened"]);
    assert_eq!(opened["workspace"]["parent"]["id"], chat.as_str());
    assert_eq!(opened["workspace"]["parent"]["kind"], "chat");
    assert_eq!(opened["workspace"]["children"][0]["name"], "below");
    let chats = fixture.ok(&["work", "--chats"]);
    let listed = chats
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == chat.as_str());
    assert_eq!(listed.unwrap()["children"][0]["name"], "opened", "{chats}");

    // A child is in its parent's repository.
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let output = add(&other, "foreign");
    assert!(!output.status.success(), "{output:?}");
    // Refused before an agent's launch is planned or queued.
    let output = fixture
        .command()
        .args(["--json", "start", other.to_str().unwrap(), "queued"])
        .args(["--agent", "missing-agent"])
        .env("SHOAL_CHAT", &chat)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));
    // Nor is registering another repository offered there.
    let unregistered = init_repo(fixture.root.path(), "third", &[("tracked", "third\n")]);
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args([
                "--text",
                "start",
                unregistered.to_str().unwrap(),
                "third-work",
            ])
            .env("SHOAL_CHAT", &chat),
        "y\n",
        true,
    );
    assert!(!output.status.success(), "{transcript}");
    assert!(!transcript.contains("Register"), "{transcript}");
    assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 2);
    let names: Vec<_> = (fixture.ok(&["work"]).as_array().unwrap().iter())
        .map(|workspace| workspace["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(!names.contains(&"foreign".to_owned()), "{names:?}");
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
            "#!/bin/sh\nshoal='{}'\n\"$shoal\" start \"$PWD\" chat-worker </dev/null >/dev/null\n\"$shoal\" internal agent-state working --space chat-worker 2>/dev/null; echo $? > \"$HOME/chat-report\"\ntouch \"$HOME/worker-opened\"\nwhile [ ! -e \"$HOME/worker-reported\" ]; do sleep 0.1; done\necho '{{\"hook_event_name\":\"PostToolUse\"}}' | \"$shoal\" messages --hook > \"$HOME/chat-hook\"\n\"$shoal\" messages --hook </dev/null > \"$HOME/chat-hook-again\"\n",
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
                    .args(["start", "--chat", "--agent", "claude"]),
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
            "chat-worker",
        ]);
        fixture.ok(&["alerts", "send", "--space", "chat-worker", "Tests pass"]);
        fs::write(home.join("worker-reported"), "").unwrap();
        chat.join().unwrap()
    });
    assert!(output.status.success(), "{output:?}\n{transcript}");
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
run() {{ name=$1; shift; "$shoal" "$@" </dev/null >/dev/null 2>"$HOME/$name.err"; echo $? > "$HOME/$name"; }}
run start start "$PWD" below
run stop stop --interrupt below
run rename work rename renamed
run alerts alerts
run repo repo add "$HOME"
run outside messages send --space outside hello
run takeover start "$PWD" --existing outside
run remove rm below --yes
"#,
            support::shoal_binary()
        ),
    );
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("ZMX_SESSION", "outer")
            .args(["start", "--chat", "--agent", "claude", "--name", "planning"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let status = |name: &str| {
        let code = fs::read_to_string(home.join(name)).unwrap();
        let error = fs::read_to_string(home.join(format!("{name}.err"))).unwrap();
        (code.trim().to_owned(), error)
    };
    // It opens, stops and removes the workspaces below it and renames itself.
    for allowed in ["start", "stop", "rename", "remove"] {
        let (code, error) = status(allowed);
        assert_eq!(code, "0", "{allowed}: {error}");
    }
    // It is refused what a workspace's agent is, and what lies outside it.
    for refused in ["alerts", "repo", "outside", "takeover"] {
        let (code, error) = status(refused);
        assert_ne!(code, "0", "{refused}: {error}");
    }
    let chats = fixture.ok(&["work", "--chats"]);
    assert_eq!(chats[0]["name"], "renamed", "{chats}");
    let names: Vec<_> = (fixture.ok(&["work"]).as_array().unwrap().iter())
        .map(|workspace| workspace["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["outside"]);
    assert!(fixture.ok(&["work", "status", "outside"])["workspace"]["parent"].is_null());
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
            "#!/bin/sh\nshoal='{}'\n\"$shoal\" --json start \"$PWD\" worker --agent codex -- 'Take the part' > \"$HOME/add.json\" 2> \"$HOME/add.err\"\n\"$shoal\" start \"$PWD\" later --agent codex --after worker 2> \"$HOME/later.err\"\necho $? > \"$HOME/later\"\n\"$shoal\" start \"$PWD\" now --agent codex --now 2> \"$HOME/now.err\"\necho $? > \"$HOME/now\"\n",
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
            .args(["start", "--chat", "--agent", "claude", "--name", "planning"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    // It reports the workspace, then the agent queued there.
    let added = fs::read_to_string(home.join("add.json")).unwrap();
    let error = fs::read_to_string(home.join("add.err")).unwrap();
    let reports: Vec<Value> = (added.lines())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("{added}\n{error}")))
        .collect();
    assert_eq!(reports.len(), 2, "{added}");
    assert_eq!(reports[0]["name"], "worker", "{added}");
    assert_eq!(reports[1]["agent"], "codex", "{added}");
    let worker = fixture.ok(&["work", "status", "worker"]);
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
    // An agent that would wait for other work, whose branch a workspace
    // created later builds on, or start at once, ignoring the queue, is
    // refused before its workspace exists.
    for name in ["later", "now"] {
        let error = fs::read_to_string(home.join(format!("{name}.err"))).unwrap();
        let code = fs::read_to_string(home.join(name)).unwrap();
        assert_eq!(code.trim(), "1", "{error}");
        assert!(!fixture.run(&["work", "status", name]).status.success());
    }
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
            .args(["start", "--chat", "--agent", "claude", "--name", "planning"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let added = fs::read_to_string(home.join("add.json")).unwrap();
    let error = fs::read_to_string(home.join("add.err")).unwrap();
    let report: Value = serde_json::from_str(added.lines().next().unwrap_or_default())
        .unwrap_or_else(|_| panic!("{added}\n{error}"));
    assert_eq!(report["name"], "12-clarify-retries", "{added}");
    let worker = fixture.ok(&["work", "status", "12-clarify-retries"]);
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
    let added = scoped_command(&fixture, "parent", &["start", repo, "--issue", "12"]);
    assert!(added.status.success(), "{added:?}");
    let worker = fixture.ok(&["work", "status", "12-clarify-retries"]);
    assert_eq!(worker["workspace"]["parent"]["name"], "parent", "{worker}");
    // Planning the issue reads it there too.
    let planned = scoped_command(
        &fixture,
        "parent",
        &[
            "start", "--mode", "plan", "--issue", "12", "--agent", "claude",
        ],
    );
    assert!(planned.status.success(), "{planned:?}");
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
             if [ ! -e \"$HOME/nested-started\" ]; then\n\
             touch \"$HOME/nested-started\"\n\
             \"$shoal\" start --chat --agent claude --name nested </dev/null > \"$HOME/nested-chat\" 2>&1\n\
             fi\n\
             echo \"$SHOAL_CHAT\" >> \"$HOME/chats-started\"\n\
             trap 'exit 0' TERM\n\
             while :; do sleep 1; done\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    fixture.add("parent");
    // A workspace's agent opens a workspace below its own...
    let repo = fixture.repo.to_str().unwrap();
    let below = scoped_command(&fixture, "parent", &["start", repo, "below"]);
    assert!(below.status.success(), "{below:?}");
    let parent = |space: &Value| space["parent"]["name"].as_str().map(str::to_owned);
    let status = fixture.ok(&["work", "status", "below"]);
    assert_eq!(parent(&status["workspace"]).as_deref(), Some("parent"));
    // ...but not a chat in its own terminal, whose processes would be its
    // own...
    let here = scoped_command(
        &fixture,
        "parent",
        &["start", "--chat", "--here", "--agent", "claude"],
    );
    assert!(!here.status.success(), "{here:?}");
    // ...nor one in the worktree of a workspace outside its tree...
    fixture.add("other");
    let args = ["start", "--in", "other", "--agent", "claude"];
    let outside = scoped_command(&fixture, "parent", &args);
    assert!(!outside.status.success(), "{outside:?}");
    // ...rather a chat below it, which opens in the background, as its agent
    // cannot hand over a terminal; that chat's agent opens one below its own.
    // The process returns once the chat holds its own scope, which outlasts
    // the process's.
    let sub = scoped_command(
        &fixture,
        "parent",
        &["start", "--chat", "--agent", "claude", "--name", "sub"],
    );
    assert!(sub.status.success(), "{sub:?}");
    wait_until("both chats started", || {
        fs::read_to_string(home.join("chats-started")).is_ok_and(|ids| ids.lines().count() == 2)
    });
    let chats = fixture.ok(&["work", "--chats"]);
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
    let removed = fixture.ok(&["rm", "parent", "--yes"]);
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
    assert_eq!(fixture.ok(&["work", "--chats"]), serde_json::json!([]));
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
             if [ -n \"$SHOAL_WORKSPACE_ID\" ]; then\n\
             \"$shoal\" --json acquire port web --reason 'serve preview' > \"$HOME/asked.tmp\" 2>&1\n\
             mv \"$HOME/asked.tmp\" \"$HOME/asked\"\n\
             while [ ! -e \"$HOME/release\" ]; do sleep 0.1; done\n\
             exit 0\n\
             fi\n\
             \"$shoal\" start \"$PWD\" deep </dev/null > \"$HOME/deep\" 2>&1\n\
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
        &["start", "--chat", "--agent", "claude", "--name", "middle"],
    );
    assert!(middle.status.success(), "{middle:?}");
    wait_until("the chat's workspace", || home.join("chat-ready").exists());
    let deep = fixture.ok(&["work", "status", "deep"]);
    assert_eq!(deep["workspace"]["parent"]["name"], "middle", "{deep}");
    let chats = fixture.ok(&["work", "--chats"]);
    let middle = chats[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(chats[0]["parent"]["name"], "top", "{chats}");

    // The running chat, not the agentless workspace above it, decides what
    // the agent of the workspace below asks for.
    let mut worker = fixture
        .command()
        .args(["claude", "deep"])
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
    fixture.ok(&["alerts", "send", "--space", "deep", "Tests pass"]);
    let messages = fixture.ok(&["messages", "middle"]);
    assert!(messages.to_string().contains("Tests pass"), "{messages}");
    // The workspace's process reaches neither the chat nor the workspace
    // above it.
    for space in ["middle", "top"] {
        let refused = scoped_command(
            &fixture,
            "deep",
            &["messages", "send", "--space", space, "hi"],
        );
        assert!(!refused.status.success(), "{space}: {refused:?}");
    }
    fs::write(home.join("release"), "").unwrap();
    assert!(worker.wait().unwrap().success());
    // Removing the top workspace takes the chat and the workspace below it.
    let removed = fixture.ok(&["rm", "top", "--yes"]);
    assert_eq!(
        removed["children"],
        serde_json::json!(["deep", "middle"]),
        "{removed}"
    );
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
    assert_eq!(fixture.ok(&["work", "--chats"]), serde_json::json!([]));
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
                .args(["start", "--chat", "--agent", "claude"]),
            "\n",
            true,
        );
        assert!(output.status.success(), "{output:?}\n{transcript}");
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
        fixture.ok(&["work", "status", "worker"])["workspace"]["path"]
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
            .env("SHOAL_WORKSPACE_ID", "worker-id")
            .env("SHOAL_WORKSPACE", "worker")
            .args(["start", "--in", "--agent", "claude"]),
        "\n",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
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
    let chats = fixture.ok(&["work", "--chats"]);
    let chat = &chats[0];
    assert_eq!(chat["name"], "worker-chat", "{chats}");
    assert_eq!(chat["workspace"]["name"], "worker", "{chats}");
    let id = chat["id"].as_str().unwrap();
    assert_eq!(fs::read_to_string(home.join("chat-id")).unwrap().trim(), id);

    // Workspace commands the chat runs act on the workspace, which its
    // scope reads but does not change.
    let scope = fs::read_to_string(home.join("chat-scope")).unwrap();
    let in_chat = |args: &[&str]| {
        fixture
            .command()
            .current_dir(&worktree)
            .env("SHOAL_CHAT", id)
            .env("SHOAL_SCOPE_TOKEN", scope.trim())
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    let status = in_chat(&["work", "status"]);
    assert!(status.status.success(), "{status:?}");
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["workspace"]["name"], "worker", "{status}");
    assert!(in_chat(&["conflicts"]).status.success());
    fs::write(
        worktree.join("tracked"),
        "changed by the workspace's agent\n",
    )
    .unwrap();
    let diff = in_chat(&["diff"]);
    assert!(diff.status.success(), "{diff:?}");
    let diff = String::from_utf8_lossy(&diff.stdout);
    assert!(diff.contains("+changed by the workspace's agent"), "{diff}");
    let view = in_chat(&["view", "--space", "worker", "--no-comments"]);
    assert!(view.status.success(), "{view:?}");
    let change = in_chat(&["branch", "base", "clear"]);
    assert!(!change.status.success(), "{change:?}");
    // A chat in a workspace takes its repository from it.
    let both = fixture.run(&[
        "start",
        "--chat",
        fixture.repo.to_str().unwrap(),
        "--workspace",
        "worker",
    ]);
    assert!(!both.status.success(), "{both:?}");

    // Each status names the other.
    assert_eq!(
        fixture.ok(&["work", "status", "worker"])["chats"][0]["name"],
        "worker-chat"
    );

    // Its agent exited; `shoal resume` restores it in the worktree.
    assert_eq!(chats[0]["restore"], "resume", "{chats}");
    fs::remove_file(home.join("chat-cwd")).unwrap();
    let (output, transcript) = fixture.interactive_command(
        fixture.command().args(["resume", "worker-chat", "--focus"]),
        "",
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(
        fs::read_to_string(home.join("chat-cwd")).unwrap().trim(),
        worktree.to_str().unwrap()
    );
    assert_eq!(fs::read_to_string(home.join("chat-id")).unwrap().trim(), id);

    let (no, prompt) = fixture.interactive(&["rm", "worker"], "n\n");
    assert!(!no.status.success());
    assert!(prompt.contains("worker-chat"), "{prompt}");
    let removed = fixture.run(&["rm", "worker", "--yes"]);
    assert!(removed.status.success(), "{removed:?}");
    assert_eq!(fixture.ok(&["work", "--chats"]), serde_json::json!([]));
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
                .args(["start", "--chat", "--agent", "claude"]),
            "\n",
            true,
        );
        assert!(output.status.success(), "{output:?}\n{transcript}");
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
            "--chat",
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
            "--chat",
            fixture.repo.to_str().unwrap(),
            "--agent",
            "codex",
            "--",
            "--search",
        ],
        "\n",
    );
    assert!(caller.status.success(), "{caller:?} {transcript}");
    let create = &herdr_calls(&fixture)[0];
    assert_eq!(create[..2], ["tab", "create"]);
    let option = |name: &str| {
        let at = create.iter().position(|arg| arg == name).unwrap();
        create[at + 1].clone()
    };
    assert_eq!(Path::new(&option("--cwd")), fixture.repo);
    assert_eq!(option("--label"), CHAT_NAME);
    assert!(create.iter().any(|arg| arg == "--focus"));
    let worker = herdr_worker(&fixture);
    let chat = worker.iter().position(|arg| arg == "start").unwrap();
    assert_eq!(
        worker[chat..],
        [
            "start", id, "--mode", "chat", "--chat", "--here", "--agent", "codex", "--name",
            CHAT_NAME, "--", "--search"
        ]
    );
    // The chat registers only once the tab runs it, so nothing is recorded
    // for `attach -` yet; the tab's call records it.
    let history = fixture.root.path().join("state/recent-workspaces.json");
    assert!(!history.exists());
    assert!(
        create.iter().any(|arg| arg == "SHOAL_SHOWN_CHAT=1"),
        "{create:?}"
    );

    // The tab's own call runs the agent in its pane.
    let (output, transcript) = fixture.interactive_command(
        herdr_worker_command(&fixture)
            .current_dir(&fixture.repo)
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p9"),
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    let chats = fixture.ok(&["work", "--chats"]);
    let recorded: Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert_eq!(
        recorded["attach"],
        serde_json::json!([chats[0]["id"]]),
        "{chats}"
    );
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
    // It runs a workspace agent's hooks without the subagent brief.
    let hooks = args[2..10].chunks(2).map(|flag| {
        assert_eq!(flag[0], "-c");
        flag[1].split_once('=').unwrap().0
    });
    assert_eq!(
        hooks.collect::<Vec<_>>(),
        [
            "hooks.PostToolUse",
            "hooks.SessionStart",
            "hooks.Stop",
            "hooks.UserPromptSubmit"
        ]
    );
    // Its sandbox lets it reach the daemon.
    assert_eq!(
        args[10..12],
        ["-c", "sandbox_workspace_write.network_access=true"]
    );
    // Codex's default launcher appends its own options after forwarded ones.
    assert_eq!(after_temp_dirs(&args[12..])[0], "--search");
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
                "start", "--chat", repo, "--agent", agent, "--here", "--", "--fast",
            ]),
            "\n",
        );
        assert!(output.status.success(), "{output:?}\n{transcript}");
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
            "#!/bin/sh\n'{}' --json view --space \"$SHOAL_CHAT\" --no-comments > \"$HOME/plan-view\"\n",
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
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let viewed: Value =
        serde_json::from_str(&fs::read_to_string(fixture.root.path().join("plan-view")).unwrap())
            .unwrap();
    assert_eq!(viewed[0]["url"], url, "{viewed}");
}

#[test]
fn chat_picks_chat_agents_and_refuses_unsupported_launches() {
    let fixture = Fixture::new();
    install_fake_agents(&fixture, &["claude", "happy"]);
    let repo = fixture.repo.to_str().unwrap();
    // Without a default agent a picker offers the installed agents that can run.
    let (output, rows) = fixture.pick(&["start", "--chat", repo, "--mode", "chat"], "0", "\n");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        rows.lines()
            .map(|row| row.split('\t').nth(1).unwrap())
            .collect::<Vec<_>>(),
        ["claude"]
    );
    // An agent that cannot run a chat is refused before the mode picker.
    let args = ["start", "--chat", repo, "--agent", "happy-claude"];
    let (output, rows) = fixture.pick(&args, "chat", "");
    assert!(!output.status.success(), "{output:?}");
    assert!(rows.is_empty(), "a picker opened: {rows}");
    let output = fixture.run(&["start", "--chat", repo, "--agent", "claude"]);
    assert!(!output.status.success());
    // The checkout has no value for a workspace's placeholders.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[agents.claude]\ncommand = ['claude', '--name', '{workspace}']\n",
    )
    .unwrap();
    let (output, transcript) = fixture.interactive_command(
        fixture.command().args([
            "start", "--chat", repo, "--agent", "claude", "--here", "--mode", "chat",
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
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let other = init_repo(home, "other", &[("tracked", "committed\n")]);
    let other_repository = fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    // The chats last while this test's process runs.
    let start = |repository: &Value, name: &str, session: Option<&str>| {
        let origin = session.map(|session| serde_json::json!({"host": "zmx", "place": session}));
        let response = fixture.request(serde_json::json!({"start_chat":{
            "repository": repository["id"], "name": name, "agent": "claude",
            "pid": std::process::id(), "origin": origin
        }}));
        assert_eq!(response["type"], "chat", "{response}");
        response["data"]["id"].as_str().unwrap().to_owned()
    };
    // Sessions are found where the chat was recorded, whatever their name.
    let talk = start(&repository, "talk", Some("second"));
    let plan = start(&repository, "plan-7", Some("first"));
    start(&other_repository, "quiet", None);
    let session = |name: &str| format!("  name={name}\tpid=1\tclients=0\n");
    fs::write(
        home.join("zmx-sessions"),
        session("first") + &session("second"),
    )
    .unwrap();
    let attached = || {
        zmx_calls(&fixture)
            .into_iter()
            .rfind(|call| call[0] == "attach")
            .unwrap()
    };

    let (output, transcript) = fixture.interactive(&["attach", "talk"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), ["attach", "second"]);
    let (output, transcript) = fixture.interactive(&["attach", "plan-7"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), ["attach", "first"]);

    // In a checkout with two chats running, the picker offers the chats with
    // sessions by name.
    let (output, rows) = fixture.pick_in(&fixture.repo, &["attach"], &talk, "");
    assert!(output.status.success(), "{output:?}");
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
    named.args(["attach", "quiet"]);
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
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let other = init_repo(home, "other", &[("tracked", "committed\n")]);
    let other_repository = fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    // The chats last while this test's process runs.
    let start = |repository: &Value, name: &str, session: Option<&str>| {
        let origin = session.map(|session| serde_json::json!({"host": "zmx", "place": session}));
        let response = fixture.request(serde_json::json!({"start_chat":{
            "repository": repository["id"], "name": name, "agent": "claude",
            "pid": std::process::id(), "origin": origin
        }}));
        assert_eq!(response["type"], "chat", "{response}");
        response["data"]["id"].as_str().unwrap().to_owned()
    };
    let talk = start(&repository, "talk", Some("talk"));
    let plan = start(&repository, "repo-plan-7", None);
    start(&other_repository, "quiet", None);
    fs::write(home.join("zmx-sessions"), "  name=talk\tpid=1\tclients=0\n").unwrap();
    let names = || -> Vec<String> {
        let chats = fixture.ok(&["work", "--chats"]);
        chats
            .as_array()
            .unwrap()
            .iter()
            .map(|chat| chat["name"].as_str().unwrap().to_owned())
            .collect()
    };

    // By name; the zmx session keeps its name, and attach finds it by the
    // chat's new one.
    let renamed = fixture.ok(&["work", "rename", "talk", "topic"]);
    assert_eq!(renamed["id"], talk.as_str());
    assert_eq!(renamed["name"], "topic");
    let (output, transcript) = fixture.interactive(&["attach", "topic"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let output = fixture.run(&["work", "rename", "repo-plan-7", "topic"]);
    assert!(!output.status.success());

    // The chat this runs in, as its agent would.
    let output = fixture
        .command()
        .env("SHOAL_CHAT", &plan)
        .args(["work", "rename", "repo-plan-8"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    // The checkout's only chat, else one from a picker.
    let output = fixture
        .command()
        .current_dir(&other)
        .args(["work", "rename", "calm"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // A chat refuses a name a workspace's title could have.
    let output = fixture
        .command()
        .current_dir(&fixture.repo)
        .args(["work", "rename", "calm", "bad/name"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let (output, rows) = fixture.pick_in(&fixture.repo, &["work", "rename", "picked"], &talk, "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows.lines().count(), 3, "{rows}");
    assert_eq!(names(), ["picked", "repo-plan-8", "calm"]);
}

#[test]
fn a_name_request_reaches_the_chats_agent() {
    let fixture = Fixture::new();
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    // The chat lasts while this test's process runs.
    let response = fixture.request(serde_json::json!({"start_chat":{
        "repository": repository["id"], "name": "talk", "agent": "claude",
        "pid": std::process::id()
    }}));
    let chat = response["data"]["id"].as_str().unwrap().to_owned();
    let output = fixture.run(&["work", "rename", "--request", "talk"]);
    assert!(output.status.success(), "{output:?}");
    // The chat's agent reads it as a message from the user, about no
    // workspace.
    let output = fixture
        .command()
        .env("SHOAL_CHAT", &chat)
        .args(["--json", "messages"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let messages: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(messages.as_array().map(Vec::len), Some(1), "{messages}");
    // Workspace processes do not reach chats.
    fixture.add("worker");
    let output = scoped_command(&fixture, "worker", &["work", "rename", "--request", "talk"]);
    assert!(!output.status.success(), "{output:?}");
}
