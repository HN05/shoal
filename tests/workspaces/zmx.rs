//! zmx sessions: tracked terminal launches, detached sessions and `shoal
//! attach`.
use super::*;

/// Wait until no session `PASSTHROUGH_ZMX` detached under `home` still runs
/// sent text. Those run in sessions of their own, which outlive the test.
pub(crate) fn wait_zmx_sessions_ended(home: &Path) {
    wait_until("zmx sessions ended", || {
        !fs::read_dir(home).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".running")
        })
    });
}

/// The `zmx list` line of session `name`, one Shoal created for `workspace`
/// in the fixture's state directory.
pub(crate) fn created_zmx_session(fixture: &Fixture, name: &str, workspace: &str) -> String {
    use sha2::Digest;

    let state = fs::canonicalize(fixture.root.path().join("state")).unwrap();
    let state: String = sha2::Sha256::digest(state.as_os_str().as_encoded_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "  name={name}\tpid=1\tshoal.created=1\tshoal.state={state}\tshoal.workspace={workspace}\n"
    )
}

pub(crate) fn zmx_calls(fixture: &Fixture) -> Vec<Vec<String>> {
    fs::read_to_string(fixture.root.path().join("zmx-calls"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn terminal_agents_run_tracked_in_a_labelled_zmx_session_unless_nested_or_detached() {
    let fixture = Fixture::new();
    let workspace = fixture.add("persist");
    let id = workspace["id"].as_str().unwrap();
    let home = fixture.root.path();
    install_test_script(
        home.join("bin/claude"),
        "#!/bin/sh\nprintf '%s %s\\n' \"${ZMX_SESSION:-none}\" \"${SHOAL_EXECUTION_ID:+tracked}\" >> \"$HOME/agent-runs\"\n",
    );
    let runs = || fs::read_to_string(home.join("agent-runs")).unwrap_or_default();

    let launch = |command: &mut Command| fixture.terminal_command(command, "", true);
    let (output, transcript) = launch(fixture.command().args(["claude", "persist"]));
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    assert_eq!(calls[0], ["list"]);
    let labels = format!("shoal.workspace={id}");
    assert_eq!(calls[1][..2], ["attach", "--labels"]);
    assert!(calls[1][2].starts_with(&format!("{labels} shoal.created=1 shoal.state=")));
    assert_eq!(calls[1][3], "persist");
    let worker = &calls[1][4..];
    let session = worker.iter().position(|arg| arg == "session").unwrap();
    assert_eq!(worker[session - 1], "internal");
    // The session names the host that opened it and closes with its workspace.
    assert_eq!(
        worker[session + 1..session + 11],
        [
            id,
            "claude",
            "--mode",
            "worker",
            "--host",
            "zmx",
            "--close-in",
            "zmx",
            "--",
            "claude"
        ]
    );
    assert_eq!(runs(), "persist tracked\n");

    // A session name in use gets the next suffix.
    fs::write(
        home.join("zmx-sessions"),
        format!("  name=persist\tpid=1\tclients=0\tshoal.workspace={id}\n"),
    )
    .unwrap();
    let (output, transcript) = launch(fixture.command().args(["claude", "persist"]));
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let attaches = |calls: Vec<Vec<String>>| calls.into_iter().filter(|call| call[0] == "attach");
    assert_eq!(
        attaches(zmx_calls(&fixture)).next_back().unwrap()[3],
        "persist-2"
    );

    // Inside a session or without a terminal, the agent runs in place.
    let before = zmx_calls(&fixture).len();
    let (output, transcript) = launch(
        fixture
            .command()
            .env("ZMX_SESSION", "outer")
            .args(["claude", "persist"]),
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let output = fixture.run(&["claude", "persist"]);
    assert!(output.status.success(), "{output:?}");
    // Watching sessions lists them on its own schedule, so only the other
    // calls are counted. The execution tells its session apart from a later
    // one under the name, and neither run opens a session.
    let calls: Vec<_> = zmx_calls(&fixture)
        .split_off(before)
        .into_iter()
        .filter(|call| call[0] != "list")
        .collect();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0][..2], ["set", "outer"]);
    assert!(calls[0].contains(&labels), "{calls:?}");
    assert_eq!(
        runs(),
        "persist tracked\npersist-2 tracked\nouter tracked\nnone tracked\n"
    );
}

#[test]
fn terminal_add_detaches_launches_with_input_and_attaches_the_rest() {
    // `shift` drops Shoal's guidance, the first argument.
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 1\n[agents.pi]\ncommand = ['sh', '-c', 'shift; echo \"$ZMX_SESSION ${SHOAL_EXECUTION_ID:+tracked} $*\" >> \"$HOME/agent-runs\"', 'agent', '{args}']\n",
    ));
    let home = fixture.root.path();
    let repo = fixture.repo.to_str().unwrap();
    let runs = || fs::read_to_string(home.join("agent-runs")).unwrap_or_default();

    // An agent with input starts in a detached session that runs the plan.
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args(["start", repo, "background", "--agent", "pi", "--", "go"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    wait_until("the detached agent's result", || {
        runs() == "background tracked go\n"
    });
    let calls = zmx_calls(&fixture);
    assert_eq!(calls[1], ["run", "background", "-d", "true"]);
    assert_eq!(calls[2], ["list"]);
    // Only sessions Shoal created carry the creation label.
    assert_eq!(calls[3][..3], ["set", "background", "shoal.created=1"]);
    assert!(calls[3][3].starts_with("shoal.state="), "{calls:?}");
    assert_eq!(calls[4][..2], ["send", "background"]);
    assert!(
        calls[4][2].starts_with("SHOAL_ENDS_SESSION=1 ")
            && calls[4][2].ends_with(
                " internal add --host zmx --close-when-done && exit; exec \"$SHELL\" -l\r"
            ),
        "{calls:?}"
    );
    let session: Value =
        serde_json::from_str(&fs::read_to_string(home.join("zmx-background.env")).unwrap())
            .unwrap();
    assert!(session["SHOAL_ADD_PLAN"].is_string(), "{session}");
    let log = fs::read_to_string(home.join("zmx-background.log")).unwrap_or_default();
    assert_eq!(runs(), "background tracked go\n", "{log}");
    let workspace = fixture.ok(&["status", "background"]);
    let id = workspace["workspace"]["id"].as_str().unwrap();
    let labels = [
        "set",
        "background",
        &format!("shoal.workspace={id}"),
        "shoal.chat=",
    ];
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with(&labels.map(str::to_owned))),
        "{calls:?}"
    );

    // An agent waiting for its first prompt attaches this terminal, which
    // detaching returns to the caller's shell where it was. Its slot must be
    // free in the daemon; the previous agent's output does not prove that.
    wait_until("the detached agent's recorded completion", || {
        fixture.ok(&["status", "background"])["executions"] == serde_json::json!([])
    });
    let directive = home.join("shell-directive");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args(["start", repo, "focused", "--agent", "pi"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    let attach = calls.iter().rfind(|call| call[0] == "attach").unwrap();
    assert_eq!(attach[..2], ["attach", "--labels"]);
    assert_eq!(attach[3], "focused");
    assert_eq!(runs(), "background tracked go\nfocused tracked \n");
    assert!(!directive.exists(), "{transcript}");

    // Without an agent the caller's shell enters the workspace.
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args(["start", repo, "shell-here", "--focus"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let workspace = fixture.ok(&["status", "shell-here"]);
    assert_eq!(
        fs::read_to_string(&directive).unwrap().trim(),
        workspace["workspace"]["path"].as_str().unwrap()
    );
}

#[test]
fn background_add_without_an_agent_opens_the_workspace_shell_in_its_session() {
    let fixture = Fixture::with_config(Some("terminal.focus = false\n"));
    let home = fixture.root.path();
    let shell = home.join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\npwd > \"$HOME/shell-cwd\"\n");
    let (output, transcript) = fixture.terminal_command(
        fixture.command().env("SHELL", &shell).args([
            "start",
            fixture.repo.to_str().unwrap(),
            "shell-only",
        ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    wait_until("the detached workspace shell", || {
        fs::read_to_string(home.join("shell-cwd")).is_ok_and(|cwd| !cwd.is_empty())
    });
    let workspace = fixture.ok(&["status", "shell-only"]);
    let log = fs::read_to_string(home.join("zmx-shell-only.log")).unwrap_or_default();
    assert_eq!(
        fs::read_to_string(home.join("shell-cwd"))
            .unwrap_or_default()
            .trim(),
        fs::canonicalize(workspace["workspace"]["path"].as_str().unwrap())
            .unwrap()
            .to_str()
            .unwrap(),
        "{log}"
    );
    let id = workspace["workspace"]["id"].as_str().unwrap();
    let labels = ["set", "shell-only", &format!("shoal.workspace={id}")];
    assert!(
        zmx_calls(&fixture)
            .iter()
            .any(|call| call.starts_with(&labels.map(str::to_owned)))
    );
}

#[test]
fn terminal_resume_all_starts_each_agent_in_a_detached_session() {
    let fixture = Fixture::new();
    let mut expected = Vec::new();
    // Sessions are named after their workspaces, not their branches.
    for branch in ["team/first", "team/second"] {
        let workspace = fixture.add(branch);
        let name = workspace["name"].as_str().unwrap().to_owned();
        assert_ne!(name, branch);
        let id = workspace["id"].as_str().unwrap().to_owned();
        let state = fixture.root.path().join("state/workspaces").join(&id);
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.recovery.json")),
            r#"{"agent":"claude"}"#,
        )
        .unwrap();
        let execution = format!("{name}-agent");
        expected.push((name, id, execution));
    }
    // The sessions cannot resume without a configured restore command; the
    // test observes what each was asked to run.
    let shell = fixture.root.path().join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\necho \"$@\" >> \"$HOME/login-shells\"\n");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHELL", &shell)
            .args(["resume", "--all", "--now"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    for (name, id, execution) in &expected {
        assert!(
            calls.contains(&vec![
                "run".into(),
                name.to_string(),
                "-d".into(),
                "true".into()
            ]),
            "{calls:?}"
        );
        let send = calls
            .iter()
            .find(|call| call.starts_with(&["send".into(), name.to_string()]))
            .unwrap();
        assert!(
            send[2].ends_with(&format!(
                " resume {id} --execution {execution} --focus --opened-in zmx --close-in zmx && exit; exec \"$SHELL\" -l\r"
            )),
            "{send:?}"
        );
    }
    // Each failed start leaves the user's own login shell to show why.
    wait_until("the failed restores' login shells", || {
        fs::read_to_string(fixture.root.path().join("login-shells"))
            .is_ok_and(|shells| shells == "-l\n-l\n")
    });
}

#[test]
fn terminal_resume_of_one_agent_runs_in_a_detached_session_unless_focused() {
    let fixture = Fixture::new();
    let workspace = fixture.add("single");
    let id = workspace["id"].as_str().unwrap().to_owned();
    let state = fixture.root.path().join("state/workspaces").join(&id);
    fs::create_dir_all(&state).unwrap();
    let record = state.join("single-agent.recovery.json");
    fs::write(&record, r#"{"agent":"claude"}"#).unwrap();
    let shell = fixture.root.path().join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\necho \"$@\" >> \"$HOME/login-shells\"\n");
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHELL", &shell)
            .args(["resume", "single", "--now"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    assert!(calls.iter().all(|call| call[0] != "attach"), "{calls:?}");
    let send = calls
        .iter()
        .find(|call| call.starts_with(&["send".into(), "single".into()]))
        .unwrap();
    assert!(
        send[2].contains(&format!(" resume {id} --execution single-agent --focus ")),
        "{send:?}"
    );

    // The restore fails without a configured restore command, leaving the
    // login shell, before the next one starts.
    wait_until("the failed restore's login shell", || {
        fixture.root.path().join("login-shells").exists()
    });

    // Focused, the agent's session takes this terminal; its restore fails
    // as the detached one did, so only the zmx calls show the path taken.
    fs::remove_file(fixture.root.path().join("zmx-calls")).unwrap();
    fs::write(&record, r#"{"agent":"claude"}"#).unwrap();
    fixture.terminal_command(
        fixture
            .command()
            .env("SHELL", &shell)
            .args(["resume", "single", "--focus"]),
        "",
        true,
    );
    let calls = zmx_calls(&fixture);
    assert!(calls.iter().any(|call| call[0] == "attach"), "{calls:?}");
    assert!(calls.iter().all(|call| call[0] != "send"), "{calls:?}");
}

#[test]
fn a_detached_session_ends_once_a_kept_completion_resolves_its_agent() {
    let fixture = Fixture::with_config(Some(&format!(
        "{UNLIMITED_AGENTS}[cleanup.done]\nenabled = false\n[agents.pi]\ncommand = ['sh', '-c', '\"$SHOAL_TEST_BINARY\" done']\n"
    )));
    let home = fixture.root.path();
    // The daemon closes the session, so it needs the fake zmx too.
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    // The fake session outlives its agent, as one with a shell would.
    fs::write(home.join("zmx-keep-sessions"), "").unwrap();
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .env("SHOAL_TEST_BINARY", support::shoal_binary())
            .args([
                "start",
                fixture.repo.to_str().unwrap(),
                "kept",
                "--agent",
                "pi",
                "--",
                "go",
            ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    // The watcher has the daemon close the session Shoal recorded creating.
    let watched = || fs::read_to_string(home.join("zmx-calls")).unwrap_or_default();
    let ended = try_wait_for(|| {
        watched()
            .lines()
            .any(|call| call == r#"["kill", "kept"]"#)
            .then_some(())
    });
    assert!(ended.is_some(), "{}\n{transcript}", watched());
    assert!(
        fixture.ok(&["status", "kept"])["workspace"]["done"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn a_detached_session_stays_open_without_close_when_done() {
    let fixture = Fixture::with_config(Some(&format!(
        "terminal.close_when_done = false\n{UNLIMITED_AGENTS}[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n"
    )));
    let (output, transcript) = fixture.terminal_command(
        fixture.command().args([
            "start",
            fixture.repo.to_str().unwrap(),
            "open",
            "--agent",
            "pi",
            "--",
            "go",
        ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let calls = zmx_calls(&fixture);
    let sent = calls
        .iter()
        .find(|call| call.starts_with(&["send".into(), "open".into()]));
    assert!(
        sent.and_then(|call| call.get(2)).is_some_and(|text| {
            text.ends_with(" internal add --host zmx && exit; exec \"$SHELL\" -l\r")
        }),
        "{calls:?}\n{transcript}"
    );
    assert!(!fixture.root.path().join("zmx-watch-calls").exists());
}

#[test]
fn a_detached_session_ends_with_its_agent_whatever_its_exit() {
    let fixture = Fixture::with_config(Some(
        "[agents.pi]\ncommand = ['sh', '-c', 'echo \"${SHOAL_ENDS_SESSION-unset}\" > \"$HOME/agent-env\"; exit 3']\n",
    ));
    let home = fixture.root.path();
    let shell = home.join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\necho \"$@\" >> \"$HOME/login-shells\"\n");
    let (output, transcript) = fixture.terminal_command(
        fixture.command().env("SHELL", &shell).args([
            "start",
            fixture.repo.to_str().unwrap(),
            "failing",
            "--agent",
            "pi",
            "--",
            "go",
        ]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    wait_until("the detached agent environment", || {
        fs::read_to_string(home.join("agent-env")).is_ok_and(|env| env == "unset\n")
    });
    let log = fs::read_to_string(home.join("zmx-failing.log")).unwrap_or_default();
    assert!(!home.join("login-shells").exists(), "{log}");
}

#[test]
fn an_idle_codex_with_updates_is_prompted_in_its_zmx_session() {
    let fixture = Fixture::new();
    fixture.add("prompted");
    fixture.ok(&[
        "messages",
        "send",
        "Rebase onto main",
        "--space",
        "prompted",
    ]);
    let home = fixture.root.path();
    // Codex's Stop hook reports the finished turn; it then waits, with a
    // generous hang guard, for the prompt Shoal types into its session.
    install_test_script(
        home.join("bin/codex"),
        format!(
            "#!/bin/sh\n'{}' internal agent-state idle >/dev/null || exit 3\n\
             for _ in $(seq 600); do [ -e \"$HOME/zmx-sent\" ] && exit 0; /bin/sleep 0.1; done\n\
             exit 4\n",
            support::shoal_binary()
        ),
    );
    let (output, transcript) =
        fixture.terminal_command(fixture.command().args(["codex", "prompted"]), "", true);
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let sent: Vec<_> = zmx_calls(&fixture)
        .into_iter()
        .filter(|call| call[0] == "send")
        .collect();
    // One prompt, pasted as a whole and submitted.
    let [prompt] = &sent[..] else {
        panic!("{sent:?}");
    };
    assert_eq!(prompt[..2], ["send", "prompted"], "{sent:?}");
    assert!(
        prompt[2].starts_with("\x1b[200~") && prompt[2].ends_with("\x1b[201~\r"),
        "{sent:?}"
    );
}

#[test]
fn zmx_agent_exit_resets_input_modes_for_the_calling_shell() {
    let fixture = Fixture::new();
    fixture.add("persist");
    install_test_script(
        fixture.root.path().join("bin/claude"),
        "#!/bin/sh\nexit 0\n",
    );
    let mut command = fixture.command();
    command
        .env("TERM", "xterm-256color")
        .env("ZMX_TEST_LEAK_INPUT_MODE", "1")
        .args(["claude", "persist"]);
    let (output, transcript) = fixture.terminal_command(&mut command, "", true);
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let enabled = transcript.rfind("\x1b[?1000h").unwrap();
    let disabled = transcript.rfind("\x1b[?1000l").unwrap();
    assert!(disabled > enabled, "zmx leaked mouse reporting mode");
}

/// A Claude that waits to be stopped, running for a workspace inside a fake
/// zmx session. Dropping it stops the agent, so a test that fails first leaves
/// nothing running.
pub(crate) struct SessionAgent<'a> {
    fixture: &'a Fixture,
    workspace: String,
    agent: Child,
}

impl Drop for SessionAgent<'_> {
    fn drop(&mut self) {
        // The wrapper ignores SIGTERM; the daemon stops it and its agent.
        let _ = self
            .fixture
            .command()
            .args(["stop", &self.workspace])
            .output();
        if try_wait_for(|| self.agent.try_wait().ok().flatten()).is_none() {
            let _ = self.agent.kill();
            let _ = self.agent.wait();
        }
    }
}

/// Run a [`SessionAgent`] for `workspace` inside the fake zmx session
/// `session`, returning once the daemon lists it as a running agent there,
/// as attach discovery reads it.
pub(crate) fn agent_in_session<'a>(
    fixture: &'a Fixture,
    workspace: &str,
    session: &str,
) -> SessionAgent<'a> {
    let bin = fixture.root.path().join("bin");
    install_test_script(bin.join("zmx"), PASSTHROUGH_ZMX);
    install_test_script(
        bin.join("claude"),
        "#!/bin/sh\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    );
    let errors = fixture.root.path().join(format!("agent-{session}.err"));
    let agent = fixture
        .command()
        .env("ZMX_SESSION", session)
        .args(["claude", workspace])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(&errors).unwrap())
        .spawn()
        .unwrap();
    let mut agent = SessionAgent {
        fixture,
        workspace: workspace.to_owned(),
        agent,
    };
    let id = fixture.ok(&["status", workspace])["workspace"]["id"].clone();
    wait_until("the agent listed in its session", || {
        if let Some(status) = agent.agent.try_wait().unwrap() {
            panic!(
                "the agent exited with {status}: {}",
                fs::read_to_string(&errors).unwrap()
            );
        }
        let agents = fixture.request(serde_json::json!("list_agents"));
        agents["data"].as_array().is_some_and(|agents| {
            agents.iter().any(|listed| {
                listed["space_id"] == id
                    && !listed["agent"].is_null()
                    && listed["origin"]["place"] == session
            })
        })
    });
    agent
}

#[test]
fn attach_selects_a_session_of_the_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.add("persist");
    fixture.add("other");
    let home = fixture.root.path();
    let attached = || {
        zmx_calls(&fixture)
            .into_iter()
            .rfind(|call| call[0] == "attach")
            .unwrap()
    };
    let (output, _) = fixture.interactive(&["attach", "persist"], "");
    assert!(!output.status.success());
    // Without a session anywhere, there is nothing to pick.
    let (output, _) = fixture.interactive(&["attach"], "");
    assert!(!output.status.success());

    // Sessions are found where the daemon recorded the agents, whatever
    // their labels say.
    let session = |name: &str| format!("  name={name}\tpid=1\tclients=0\tshoal.workspace=w0\n");
    fs::write(
        home.join("zmx-sessions"),
        session("persist") + &session("other"),
    )
    .unwrap();
    let persist = agent_in_session(&fixture, "persist", "persist");
    let other = agent_in_session(&fixture, "other", "other");
    let (output, transcript) = fixture.interactive(&["attach", "persist"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), ["attach", "persist"]);
    // The picker leaves out a workspace without a session.
    let idle = fixture.add("idle");
    let (output, rows) = fixture.pick(&["attach"], workspace["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let idle = idle["id"].as_str().unwrap();
    assert!(!rows.iter().any(|row| row.starts_with(idle)), "{rows:?}");

    let second = agent_in_session(&fixture, "persist", "persist-2");
    fs::write(
        home.join("zmx-sessions"),
        session("persist") + &session("persist-2"),
    )
    .unwrap();
    let (output, rows) = fixture.pick(&["attach", "persist"], "1", "");
    assert!(output.status.success(), "{output:?} {rows}");
    assert_eq!(attached(), ["attach", "persist-2"]);
    let (output, transcript) =
        fixture.interactive(&["attach", "persist", "--session", "persist"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), ["attach", "persist"]);
    let (output, transcript) =
        fixture.interactive(&["attach", "persist", "--session", "other"], "");
    assert!(!output.status.success(), "{transcript}");

    let output = fixture.run(&["attach", "persist"]);
    assert!(!output.status.success());
    drop((persist, second, other));
}

#[test]
fn attach_previous_returns_to_the_session_last_detached_from() {
    let fixture = Fixture::new();
    fixture.add("persist");
    fixture.add("other");
    let home = fixture.root.path();
    let session = |name: &str| format!("  name={name}\tpid=1\tclients=0\tshoal.workspace=w0\n");
    fs::write(
        home.join("zmx-sessions"),
        session("persist") + &session("other"),
    )
    .unwrap();
    let persist = agent_in_session(&fixture, "persist", "persist");
    let other = agent_in_session(&fixture, "other", "other");
    let attached = || {
        zmx_calls(&fixture)
            .into_iter()
            .rfind(|call| call[0] == "attach")
            .unwrap()
    };
    let (output, transcript) = fixture.interactive(&["attach", "other"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    // A session that ends while attached is not one to return to.
    fs::write(home.join("zmx-attach-ends-session"), "").unwrap();
    let (output, transcript) = fixture.interactive(&["attach", "persist"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    fs::remove_file(home.join("zmx-attach-ends-session")).unwrap();
    fs::write(
        home.join("zmx-sessions"),
        session("persist") + &session("other"),
    )
    .unwrap();
    let (output, transcript) = fixture.interactive(&["attach", "-"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), ["attach", "other"]);
    drop((persist, other));
}

#[test]
fn attach_previous_returns_to_a_running_chat() {
    let fixture = Fixture::new();
    fixture.add("persist");
    let home = fixture.root.path();
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let persist = agent_in_session(&fixture, "persist", "persist");
    // The sleep stands in for the chat's agent.
    let mut chat_agent = Command::new("sleep").arg("600").spawn().unwrap();
    let response = fixture.request(serde_json::json!({"start_chat":{
        "repository": repository["id"], "name": "talk", "agent": "claude",
        "pid": chat_agent.id(), "origin": {"host": "zmx", "place": "chat"}
    }}));
    assert_eq!(response["type"], "chat", "{response}");
    fs::write(
        home.join("zmx-sessions"),
        "  name=persist\tpid=1\tclients=0\tshoal.workspace=w0\n  name=chat\tpid=1\tclients=0\n",
    )
    .unwrap();
    let previous = || {
        let (output, transcript) = fixture.interactive(&["attach", "-"], "");
        assert!(output.status.success(), "{output:?}\n{transcript}");
        zmx_calls(&fixture)
            .into_iter()
            .rfind(|call| call[0] == "attach")
            .unwrap()
    };
    for target in ["persist", "talk"] {
        let (output, transcript) = fixture.interactive(&["attach", target], "");
        assert!(output.status.success(), "{output:?}\n{transcript}");
    }
    assert_eq!(previous(), ["attach", "chat"]);
    // A chat whose agent exited is passed over, as a removed workspace is.
    chat_agent.kill().unwrap();
    chat_agent.wait().unwrap();
    assert_eq!(previous(), ["attach", "persist"]);
    drop(persist);
}

#[test]
fn attach_previous_returns_to_a_session_an_agent_launch_left_running() {
    let fixture = Fixture::new();
    fixture.add("launched");
    fixture.add("other");
    let home = fixture.root.path();
    let session = |name: &str| format!("  name={name}\tpid=1\tclients=0\tshoal.workspace=w0\n");
    let listed = session("launched") + &session("other");
    fs::write(home.join("zmx-sessions"), &listed).unwrap();
    let launched = agent_in_session(&fixture, "launched", "launched");
    let other = agent_in_session(&fixture, "other", "other");
    let (output, transcript) = fixture.interactive(&["attach", "other"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    // An agent launched here runs in a new session this terminal attaches
    // to, which outlives the detach.
    install_test_script(home.join("bin/claude"), "#!/bin/sh\nexit 0\n");
    fs::write(home.join("zmx-attach-detaches"), "").unwrap();
    let mut launch = fixture.command();
    launch.args(["claude", "launched"]);
    let (output, transcript) = fixture.terminal_command(&mut launch, "", true);
    assert!(output.status.success(), "{output:?}\n{transcript}");
    fs::remove_file(home.join("zmx-attach-detaches")).unwrap();
    fs::write(home.join("zmx-sessions"), &listed).unwrap();
    let (output, transcript) = fixture.interactive(&["attach", "-"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let attached = zmx_calls(&fixture)
        .into_iter()
        .rfind(|call| call[0] == "attach")
        .unwrap();
    assert_eq!(attached, ["attach", "launched"]);
    drop((launched, other));
}

#[test]
fn attach_reaches_a_session_shoal_opened_for_a_workspace_without_an_agent() {
    let fixture = Fixture::new();
    let workspace = fixture.add("failed");
    fixture.add("finished");
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    // A detached launch whose setup failed keeps its shell, which no
    // execution records.
    let response = fixture.request(serde_json::json!({"record_owned_place":{
        "workspace": "failed", "origin": {"host": "zmx", "place": "failed"}
    }}));
    assert_eq!(response["type"], "ok", "{response}");
    // The record of a session that has ended outlives it.
    let response = fixture.request(serde_json::json!({"record_owned_place":{
        "workspace": "finished", "origin": {"host": "zmx", "place": "finished"}
    }}));
    assert_eq!(response["type"], "ok", "{response}");
    fs::write(
        fixture.root.path().join("zmx-sessions"),
        "  name=failed\tpid=1\tclients=0\n",
    )
    .unwrap();
    let attached = || {
        zmx_calls(&fixture)
            .into_iter()
            .rfind(|call| call[0] == "attach")
            .unwrap()
    };
    let (output, transcript) = fixture.interactive(&["attach", "failed"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), ["attach", "failed"]);
    // The picker offers the workspace whose session is open, and only it.
    let (output, rows) = fixture.pick(&["attach"], workspace["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(attached(), ["attach", "failed"]);
    assert_eq!(
        fixture.ok(&["status", "failed"])["sessions"],
        serde_json::json!([{"name": "failed", "agent": null, "clients": 0}])
    );
}

#[test]
fn a_later_session_under_a_recorded_name_is_not_the_recorded_one() {
    let fixture = Fixture::new();
    fixture.add("persist");
    let sessions = fixture.root.path().join("zmx-sessions");
    fs::write(&sessions, "  name=persist\tpid=1\tcreated=10\tclients=0\n").unwrap();
    let agent = agent_in_session(&fixture, "persist", "persist");
    let listed = || fixture.ok(&["status", "persist"])["sessions"].clone();
    assert_eq!(
        listed(),
        serde_json::json!([{"name": "persist", "agent": "claude", "clients": 0}])
    );
    // A record left by a wrapper that crashed outlives its session; another
    // session under the name is not the recorded one.
    fs::write(&sessions, "  name=persist\tpid=2\tcreated=20\tclients=0\n").unwrap();
    assert_eq!(listed(), serde_json::json!([]));
    drop(agent);
}

#[test]
fn status_lists_workspace_sessions_and_survives_a_failed_listing() {
    let fixture = Fixture::new();
    fixture.add("persist");
    let home = fixture.root.path();
    fs::write(
        home.join("zmx-sessions"),
        "  name=persist\tpid=1\tclients=1\n  name=other\tpid=2\tclients=0\n",
    )
    .unwrap();
    let agent = agent_in_session(&fixture, "persist", "persist");
    let status = fixture.ok(&["status", "persist"]);
    assert_eq!(
        status["sessions"],
        serde_json::json!([{"name": "persist", "agent": "claude", "clients": 1}])
    );

    install_test_script(home.join("bin/zmx"), "#!/bin/sh\necho broken >&2\nexit 1\n");
    let status = fixture.ok(&["status", "persist"]);
    assert_eq!(status["sessions"], serde_json::json!([]));
    assert!(status["sessions_error"].is_string(), "{status}");
    // The agent stops through the daemon, which reads zmx again.
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    drop(agent);
}

#[test]
fn status_reports_a_session_zmx_cannot_reach_instead_of_dropping_it() {
    let fixture = Fixture::new();
    fixture.add("persist");
    let sessions = fixture.root.path().join("zmx-sessions");
    fs::write(&sessions, "  name=persist\tpid=1\tclients=0\n").unwrap();
    let agent = agent_in_session(&fixture, "persist", "persist");
    // zmx 1.x lists a session an older zmx started without its labels.
    fs::write(
        &sessions,
        "  name=persist\terr=Unexpected\tstatus=unreachable\n",
    )
    .unwrap();
    let status = fixture.ok(&["status", "persist"]);
    assert_eq!(status["sessions"], serde_json::json!([]));
    assert!(status["sessions_error"].is_string(), "{status}");
    fs::write(&sessions, "  name=persist\tpid=1\tclients=0\n").unwrap();
    drop(agent);
}

#[test]
fn terminal_agent_launches_refuse_a_missing_zmx_before_creating_anything() {
    let fixture = Fixture::new();
    install_fake_agents(&fixture, &["claude"]);
    let path = fixture.interactive_path();
    fs::remove_file(fixture.root.path().join("runner-bin/zmx")).unwrap();
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args(["start", repo, "no-zmx", "--agent", "claude"]),
        "",
        true,
    );
    assert!(!output.status.success(), "{transcript}");
    assert!(!fixture.run(&["status", "no-zmx"]).status.success());

    // Without a terminal the agent starts detached, which needs zmx too,
    // unless --here runs it in place.
    let start = |here: &[&str]| {
        fixture
            .command()
            .args(["start", repo, "no-zmx", "--agent", "claude"])
            .args(here)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let output = start(&[]);
    assert!(!output.status.success(), "{output:?}");
    assert!(!fixture.run(&["status", "no-zmx"]).status.success());
    let output = start(&["--here"]);
    assert!(output.status.success(), "{output:?}");
}
