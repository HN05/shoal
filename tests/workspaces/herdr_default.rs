//! Herdr as the default host: launches from a terminal no host serves open
//! tabs in the Herdr workspace `[herdr] workspace_name` names, against the
//! fake `herdr`.
use super::*;

/// The repository's name, which the default workspace name takes: its
/// checkout's directory name.
fn repository_name(fixture: &Fixture) -> String {
    fixture
        .repo
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Run `args` from a plain terminal, outside Herdr.
fn plain_terminal(fixture: &Fixture, args: &[&str]) -> (Output, String) {
    fixture.terminal_command(fixture.command().args(args), "", true)
}

/// The `herdr` calls that start with `prefix`.
fn calls_of(fixture: &Fixture, prefix: &[&str]) -> Vec<Vec<String>> {
    herdr_calls(fixture)
        .into_iter()
        .filter(|call| {
            call.starts_with(&prefix.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
        })
        .collect()
}

#[test]
fn background_additions_open_tabs_in_the_repository_workspace() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\nterminal.default_host = 'herdr'\nterminal.focus = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    let repo = fixture.repo.to_str().unwrap();
    for name in ["first", "second"] {
        let (output, transcript) = plain_terminal(&fixture, &["add", repo, name, "--agent", "pi"]);
        assert!(output.status.success(), "{output:?} {transcript}");
        assert!(
            transcript.contains(&format!("Started {name} in a background Herdr tab")),
            "{transcript}"
        );
    }
    // The workspace is created once, then found by its label.
    let created = calls_of(&fixture, &["workspace", "create"]);
    assert_eq!(created.len(), 1, "{created:?}");
    let label = created[0].iter().skip_while(|arg| *arg != "--label").nth(1);
    assert_eq!(label, Some(&repository_name(&fixture)));
    let tabs = calls_of(&fixture, &["tab", "create"]);
    assert_eq!(tabs.len(), 2);
    for tab in tabs {
        assert_eq!(tab[2..4], ["--workspace", "w7"], "{tab:?}");
        assert!(tab.iter().any(|arg| arg == "--no-focus"), "{tab:?}");
    }
    assert!(
        !fs::read_to_string(fixture.root.path().join("zmx-calls"))
            .unwrap_or_default()
            .contains("attach")
    );
}

#[test]
fn a_fixed_workspace_name_is_reused_as_herdr_lists_it() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\nterminal.default_host = 'herdr'\nterminal.focus = false\n[terminal.herdr]\nworkspace_name = 'agents'\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    fs::write(
        fixture.root.path().join("herdr-workspaces"),
        r#"[{"workspace_id": "w3", "label": "agents"}]"#,
    )
    .unwrap();
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = plain_terminal(&fixture, &["add", repo, "fixed", "--agent", "pi"]);
    assert!(output.status.success(), "{output:?} {transcript}");
    assert!(calls_of(&fixture, &["workspace", "create"]).is_empty());
    assert_eq!(
        calls_of(&fixture, &["tab", "create"])[0][2..4],
        ["--workspace", "w3"]
    );
}

#[test]
fn focused_starts_attach_the_terminal_to_their_herdr_tab() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\nterminal.default_host = 'herdr'\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    fixture.add("agent");
    let repo = fixture.repo.to_str().unwrap();
    for args in [
        vec!["add", repo, "shell"],
        vec!["chat", repo, "--name", "talk", "--agent", "pi"],
        vec!["pi", "agent"],
    ] {
        fs::remove_file(fixture.root.path().join("herdr-calls")).unwrap_or_default();
        let (output, transcript) = plain_terminal(&fixture, &args);
        assert!(output.status.success(), "{args:?}: {output:?} {transcript}");
        let tab = &calls_of(&fixture, &["tab", "create"])[0];
        assert!(tab.iter().any(|arg| arg == "--focus"), "{args:?}: {tab:?}");
        // The tab's pane runs the start, then the terminal shows its tab.
        let changes: Vec<_> = herdr_changes(&fixture)
            .into_iter()
            .filter(|call| call[0] != "workspace")
            .map(|call| call[..2].join(" "))
            .collect();
        assert_eq!(
            changes,
            [
                "tab create",
                "pane run",
                "tab focus",
                "session list",
                "session attach"
            ],
            "{args:?}"
        );
        assert_eq!(
            calls_of(&fixture, &["session", "attach"])[0],
            ["session", "attach", "default"]
        );
    }
    assert!(
        !fs::read_to_string(fixture.root.path().join("zmx-calls"))
            .unwrap_or_default()
            .contains("attach")
    );
}

#[test]
fn a_herdr_only_herdr_bin_path_names_serves_a_focused_start() {
    let fixture = Fixture::with_config(Some("terminal.default_host = 'herdr'\n"));
    install_fake_herdr(&fixture);
    let home = fixture.root.path();
    let elsewhere = home.join("herdr-elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::rename(home.join("bin/herdr"), elsewhere.join("herdr")).unwrap();
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args(["add", repo, "shell"])
            .env("HERDR_BIN_PATH", elsewhere.join("herdr")),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    assert_eq!(
        calls_of(&fixture, &["session", "attach"]),
        [["session", "attach", "default"]]
    );
}

#[test]
fn a_start_queued_outside_herdr_keeps_the_herdr_session_it_chose() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 1\nterminal.default_host = 'herdr'\n[machine.overload.disk]\nenabled = false\n[commands]\nwaiter = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    ));
    install_fake_herdr(&fixture);
    let repo = fixture.repo.to_str().unwrap();
    // Without a terminal, the first agent runs in place and takes the slot.
    let mut first = fixture
        .command()
        .args(["add", repo, "first", "--agent", "waiter"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the first agent start", || {
        fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "first" && workspace["running"] == true)
            && fixture.ok(&["queue"]) == serde_json::json!([])
    });
    let output = fixture
        .command()
        .env("HERDR_SESSION", "work")
        .args(["--json", "add", repo, "second", "--agent", "waiter"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let queued: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(queued["state"], "queued", "{queued}");
    fixture.ok(&["stop", "first"]);
    first.wait().unwrap();
    fixture.ok(&["rm", "first", "--yes"]);
    // The daemon's worker opens the tab on the session the caller chose.
    let sessions = fixture.root.path().join("herdr-list-sessions");
    wait_until("the queued start to look up its workspace", || {
        fs::read_to_string(&sessions).is_ok_and(|looked_up| looked_up.ends_with('\n'))
    });
    assert_eq!(fs::read_to_string(&sessions).unwrap(), "work\n");
    wait_until("the queued start to open its tab", || {
        !calls_of(&fixture, &["tab", "create"]).is_empty()
    });
}

#[test]
fn demos_without_a_herdr_pane_open_in_the_repository_workspace() {
    let fixture = Fixture::with_config(Some("terminal.default_host = 'herdr'\n"));
    install_fake_herdr(&fixture);
    let _panes = RunningPanes::start(&fixture);
    fixture.add("tried");
    let started = fixture.ok(&[
        "demo",
        "start",
        "--workspace",
        "tried",
        "--",
        "sleep",
        "600",
    ]);
    assert_eq!(
        (&started["host"], &started["group"]),
        (&"herdr".into(), &"w7".into()),
        "{started}"
    );
    let tab = &calls_of(&fixture, &["tab", "create"])[0];
    assert_eq!(tab[2..4], ["--workspace", "w7"], "{tab:?}");
    assert!(tab.iter().any(|arg| arg == "--no-focus"), "{tab:?}");
    assert_eq!(calls_of(&fixture, &["pane", "run", "w1:p9"]).len(), 1);
    fixture.ok(&["demo", "stop", "--workspace", "tried"]);
}

#[test]
fn a_default_host_config_turns_off_leaves_zmx_alone() {
    let fixture = Fixture::with_config(Some(
        "terminal.default_host = 'herdr'\n[terminal.herdr]\nenabled = false\n[machine.ai.droid]\ncommand = ['fake-droid', '{args}']\n",
    ));
    let bin = fixture.root.path().join("bin");
    install_test_script(bin.join("zmx"), PASSTHROUGH_ZMX);
    install_test_script_with_mode(bin.join("fake-droid"), "#!/bin/sh\nexit 0\n", 0o700);
    fixture.add("tried");
    // A demo without a host runs as a background process.
    let started = fixture.ok(&[
        "demo",
        "start",
        "--workspace",
        "tried",
        "--",
        "sleep",
        "600",
    ]);
    assert_eq!(started["host"], "terminal", "{started}");
    fixture.ok(&["demo", "stop", "--workspace", "tried"]);
    // A queued start without a host is refused before the child exists.
    let refused = fixture.run(&["split", "child", "--workspace", "tried", "--agent", "droid"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("no terminal host can start queued agents"),
        "{stderr}"
    );
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
    assert_eq!(zmx_calls(&fixture), Vec::<Vec<String>>::new());
}

/// Commands the fake `herdr` runs in its panes, as Herdr would, until the
/// test ends: closing a pane, which ends them in Herdr, does not in the fake.
struct RunningPanes(PathBuf);

impl RunningPanes {
    fn start(fixture: &Fixture) -> Self {
        let root = fixture.root.path();
        fs::write(root.join("herdr-runs-panes"), "").unwrap();
        Self(root.join("herdr-pane-groups"))
    }
}

impl Drop for RunningPanes {
    fn drop(&mut self) {
        for group in fs::read_to_string(&self.0).unwrap_or_default().lines() {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{group}")])
                .status();
        }
    }
}
