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
        "max_agents = 'none'\ndefault_host = 'herdr'\nfocus = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
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
        "max_agents = 'none'\ndefault_host = 'herdr'\nfocus = false\n[herdr]\nworkspace_name = 'agents'\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
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
        "max_agents = 'none'\ndefault_host = 'herdr'\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
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
fn demos_without_a_herdr_pane_open_in_the_repository_workspace() {
    let fixture = Fixture::with_config(Some("default_host = 'herdr'\n"));
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
