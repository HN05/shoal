//! Herdr as a terminal host: the tabs Shoal opens, their labels and focus,
//! and closing them.
use super::forge::github_issue;
use super::start::strip_shoal_arguments;
use super::*;

pub(crate) fn install_fake_herdr(fixture: &Fixture) {
    install_test_script(
        fixture.root.path().join("bin/herdr"),
        r#"#!/usr/bin/env python3
import json, os, shlex, sys
from pathlib import Path
root = Path(os.environ['HOME'])
args = sys.argv[1:]
# Place watchers poll `pane get` and `tab get`; keep reads out of the recorded
# actions.
log = 'herdr-gets' if args[1:2] == ['get'] else 'herdr-calls'
# A run's worker is written before the run is logged, so a test that sees
# the run reads its worker.
if args[:2] == ['pane', 'run']:
    (root / 'herdr-worker').write_text(json.dumps(shlex.split(args[3])))
with (root / log).open('a') as calls:
    calls.write(json.dumps(args) + '\n')
if args[:2] == ['tab', 'create']:
    print(json.dumps({'result': {'tab': {'tab_id': 'w1:t9'}, 'root_pane': {'pane_id': 'w1:p9'}}}))
elif args[:2] == ['pane', 'run']:
    if (root / 'herdr-runs-panes').exists():
        import subprocess
        pane = subprocess.Popen(['sh', '-c', args[3]], start_new_session=True,
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        with (root / 'herdr-pane-groups').open('a') as groups:
            groups.write('%d\n' % pane.pid)
elif args[:2] == ['tab', 'rename'] and (root / 'fail-rename').exists():
    sys.exit('rename-failed')
elif args[:2] == ['tab', 'rename']:
    path = root / 'herdr-tab-labels'
    labels = json.loads(path.read_text()) if path.exists() else {}
    path.write_text(json.dumps({**labels, args[2]: args[3]}))
elif args[:2] == ['pane', 'close'] and (root / 'herdr-unreachable').exists():
    print(json.dumps({'error': {'code': 'server_not_running'}}), file=sys.stderr)
    sys.exit(1)
elif args[1:2] == ['get']:
    # Panes `w1:pN` and `w1:pN.M` are in tab `w1:tN`, and go with it.
    if (root / 'herdr-unreachable').exists():
        print(json.dumps({'error': {'code': 'server_not_running'}}), file=sys.stderr)
        sys.exit(1)
    kind, id = args[0], args[2]
    tab = id.replace(':p', ':t').split('.')[0]
    def listed(name, value):
        path = root / name
        return path.exists() and value in path.read_text().split()
    if not id.split(':')[-1].startswith(kind[0]) or listed('closed-tabs', tab) \
            or listed('closed-panes', id):
        print(json.dumps({'error': {'code': kind + '_not_found'}}), file=sys.stderr)
        sys.exit(1)
    if kind == 'pane':
        print(json.dumps({'result': {'pane': {'pane_id': id, 'tab_id': tab}}}))
    else:
        path = root / 'herdr-tab-labels'
        labels = json.loads(path.read_text()) if path.exists() else {}
        print(json.dumps({'result': {'tab': {'tab_id': id, 'label': labels.get(id, 'tab ' + id)}}}))
elif args[:2] == ['workspace', 'list']:
    with (root / 'herdr-list-sessions').open('a') as sessions:
        sessions.write(os.environ.get('HERDR_SESSION', '') + '\n')
    path = root / 'herdr-workspaces'
    workspaces = json.loads(path.read_text()) if path.exists() else []
    print(json.dumps({'result': {'workspaces': workspaces}}))
elif args[:2] == ['workspace', 'create']:
    path = root / 'herdr-workspaces'
    workspaces = json.loads(path.read_text()) if path.exists() else []
    created = {'workspace_id': 'w%d' % (len(workspaces) + 7), 'label': args[args.index('--label') + 1]}
    path.write_text(json.dumps(workspaces + [created]))
    print(json.dumps({'result': {'workspace': created}}))
elif args[:2] == ['session', 'list']:
    print(json.dumps({'sessions': [
        {'name': 'default', 'default': True, 'socket_path': '/herdr/default/herdr.sock'},
        {'name': 'work', 'default': False, 'socket_path': '/herdr/work/herdr.sock'}]}))
elif args[:2] == ['pane', 'process-info']:
    idle = root / 'idle-panes'
    at_prompt = idle.exists() and args[3] in idle.read_text().split()
    processes = {'shell_pid': 1, 'foreground_process_group_id': 1 if at_prompt else 2}
    print(json.dumps({'result': {'process_info': processes}}))
print('{}') if args[:2] not in (['tab', 'create'], ['tab', 'get'], ['pane', 'get'], ['pane', 'process-info'], ['workspace', 'list'], ['workspace', 'create'], ['session', 'list']) else None
"#,
    );
}

pub(crate) fn herdr_call(fixture: &Fixture, args: &[&str], answer: &str) -> (Output, String) {
    fixture.interactive_command(
        fixture
            .command()
            .args(args)
            .env("HERDR_ENV", "1")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("HERDR_TAB_ID", "w1:t1")
            .env("HERDR_PANE_ID", "w1:p1"),
        answer,
    )
}

/// The worker the newest tab runs, once it runs one: the queue opens the
/// tab of a start whose agent it starts in the background, and an earlier
/// tab's worker stays until the new one replaces it.
pub(crate) fn herdr_worker(fixture: &Fixture) -> Vec<String> {
    let root = fixture.root.path();
    let worker = support::wait::wait_for("the newest herdr tab's worker", || {
        let calls = fs::read_to_string(root.join("herdr-calls")).unwrap_or_default();
        let count = |prefix: &str| {
            calls
                .lines()
                .filter(|call| call.starts_with(prefix))
                .count()
        };
        let ran = count(r#"["pane", "run""#) >= count(r#"["tab", "create""#);
        ran.then(|| fs::read(root.join("herdr-worker")).ok())
            .flatten()
    });
    serde_json::from_slice(&worker).unwrap()
}

/// The worker Herdr runs in the created tab, with that tab's environment.
pub(crate) fn herdr_worker_command(fixture: &Fixture) -> Command {
    let mut command = fixture.command();
    command
        .env_remove("SHOAL_STATE_DIR")
        .args(&herdr_worker(fixture)[1..])
        .env("HERDR_ENV", "1")
        .env("HERDR_WORKSPACE_ID", "w1")
        .env("HERDR_TAB_ID", "w1:t9")
        .env("HERDR_PANE_ID", "w1:p9");
    let calls = herdr_calls(fixture);
    let create = calls
        .iter()
        .rfind(|call| call[..2] == ["tab", "create"])
        .unwrap();
    for pair in create.windows(2).filter(|pair| pair[0] == "--env") {
        let (name, value) = pair[1].split_once('=').unwrap();
        command.env(name, value);
    }
    command
}

/// The file the tab's shell follows to enter the workspace its worker added.
fn herdr_tab_directive(fixture: &Fixture) -> PathBuf {
    herdr_calls(fixture)[0]
        .iter()
        .find_map(|arg| arg.strip_prefix("SHOAL_SHELL_DIRECTIVE="))
        .map(PathBuf::from)
        .unwrap()
}

/// The `herdr` calls that change something, without the pane reads Shoal
/// makes to find where a place runs and whether its shell is at its prompt.
pub(crate) fn herdr_changes(fixture: &Fixture) -> Vec<Vec<String>> {
    herdr_calls(fixture)
        .into_iter()
        .filter(|call| call[..2] != ["pane", "get"] && call[..2] != ["pane", "process-info"])
        .collect()
}

/// How many tabs Shoal opened so far.
pub(crate) fn herdr_tabs(fixture: &Fixture) -> usize {
    let calls = fs::read_to_string(fixture.root.path().join("herdr-calls")).unwrap_or_default();
    (calls.lines())
        .filter(|call| call.starts_with(r#"["tab", "create""#))
        .count()
}

/// Wait until Shoal opened a tab beyond the `before` it had opened, as the
/// queue does for a start whose agent it starts in the background.
pub(crate) fn wait_for_tab(fixture: &Fixture, before: usize) {
    support::wait::wait_until("a new herdr tab", || herdr_tabs(fixture) > before);
}

/// The `herdr` calls made so far, once there is one: a start whose agent
/// the queue starts in the background opens its tab once the queue does.
/// The fake creates its log before it writes a call, so only whole lines
/// count.
pub(crate) fn herdr_calls(fixture: &Fixture) -> Vec<Vec<String>> {
    let calls = fixture.root.path().join("herdr-calls");
    support::wait::wait_for("a herdr call", || {
        fs::read_to_string(&calls)
            .ok()
            .filter(|text| text.ends_with('\n'))
    })
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}

#[test]
fn herdr_handoff_preserves_choices_and_literal_arguments() {
    let fixture = Fixture::with_config(Some(
        "[agents.pi]\ncommand = ['sh', '-c', 'printf \"%s\\0\" \"$@\" > \"$HOME/agent-args\"; echo \"${SHOAL_PLACE_PLAN-unset}\" > \"$HOME/agent-plan\"; exit 7', 'agent', '{args}']\n",
    ));
    install_fake_herdr(&fixture);
    let directive = fixture.root.path().join("caller-directive");
    fs::write(&directive, "unchanged").unwrap();
    let args = [
        "start",
        fixture.repo.to_str().unwrap(),
        "--branch",
        "herdr-agent",
        "--agent",
        "pi",
        "--",
        "two words",
        "quote ' ; $(touch injected)",
    ];
    let (caller, _) = fixture.interactive_command(
        fixture
            .command()
            .args(args)
            .env("HERDR_ENV", "1")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .env("SHOAL_OUTPUT", "text"),
        "",
    );
    assert!(caller.status.success(), "{}", printed(&caller));
    // The workspace is recorded at once, and the queue opens its agent's tab.
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
    assert_eq!(fs::read_to_string(directive).unwrap(), "unchanged");
    let calls = support::wait::wait_for("the tab running its worker", || {
        let calls = herdr_changes(&fixture);
        (calls.len() >= 2).then_some(calls)
    });
    assert_eq!(&calls[0][..3], ["tab", "create", "--workspace"]);
    assert!(calls[0].iter().any(|arg| arg == "--no-focus"));
    // The tab gets the caller's output setting, not the Herdr server's.
    assert!(calls[0].iter().any(|arg| arg == "SHOAL_OUTPUT=text"));
    assert_eq!(&calls[1][..3], ["pane", "run", "w1:p9"]);
    let state = fixture.root.path().join("state");
    assert_eq!(
        &herdr_worker(&fixture)[1..],
        [
            "--state-dir",
            state.to_str().unwrap(),
            "--text",
            "internal",
            "place",
            "--host",
            "herdr",
            "--close-when-done"
        ]
    );
    let (agent, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert_eq!(
        agent.status.code(),
        Some(7),
        "{} {transcript}",
        printed(&agent)
    );
    // The tab's shell, not the caller's, enters the workspace after the agent.
    let tab_directive = herdr_tab_directive(&fixture);
    let workspace = fixture.ok(&["status", &fixture.id("herdr-agent")]);
    assert_eq!(
        fs::read_to_string(&tab_directive).unwrap(),
        format!("{}\n", workspace["workspace"]["path"].as_str().unwrap())
    );
    fs::remove_file(tab_directive).unwrap();
    assert_eq!(
        strip_shoal_arguments(
            &fs::read_to_string(fixture.root.path().join("agent-args")).unwrap(),
            '\0'
        ),
        "two words\0quote ' ; $(touch injected)\0"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("agent-plan")).unwrap(),
        "unset\n"
    );
    assert!(!fixture.root.path().join("injected").exists());
    assert_eq!(herdr_changes(&fixture).len(), 3);
}

#[test]
fn herdr_resume_all_reuses_stopped_panes_and_labels_new_tabs_as_add_does() {
    let fixture = Fixture::with_config(Some("[terminal.herdr]\ntab_name = 'resumed {branch}'\n"));
    install_fake_herdr(&fixture);
    // The first agent's pane is back at its prompt; the second's is busy.
    fs::write(fixture.root.path().join("idle-panes"), "w1:p5\n").unwrap();
    let mut expected = Vec::new();
    for (name, pane) in [("first", "w1:p5"), ("second", "w1:p6")] {
        let workspace = fixture.add(name);
        let id = workspace["id"].as_str().unwrap();
        let state = fixture.root.path().join("state/workspaces").join(id);
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.recovery.json")),
            format!(r#"{{"agent":"claude","mode":"worker","origin":{{"host":"herdr","place":"{pane}"}}}}"#),
        )
        .unwrap();
        expected.push((
            workspace["path"].as_str().unwrap().to_owned(),
            id.to_owned(),
            format!("{name}-agent"),
        ));
    }
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .args(["resume", "--all", "--now"])
            .env("HERDR_ENV", "1")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("HERDR_TAB_ID", "w1:t1")
            .env("HERDR_PANE_ID", "w1:p1")
            .env("SHOAL_OUTPUT", "text"),
        "",
    );
    assert!(output.status.success(), "{} {transcript}", printed(&output));
    // The daemon's queue starts both restores at once, each through a
    // worker of its own, so their calls interleave.
    let run_in = |calls: &[Vec<String>], pane: &str| {
        (calls.iter())
            .find(|call| call[..3] == ["pane", "run", pane])
            .cloned()
    };
    let calls = support::wait::wait_for("both restores to run", || {
        let calls = herdr_calls(&fixture);
        let both = run_in(&calls, "w1:p5").is_some() && run_in(&calls, "w1:p9").is_some();
        both.then_some(calls)
    });
    let resumed = |run: &[String], (_, id, execution): &(String, String, String), close: &str| {
        let resume = format!(" resume {id} --execution {execution} --focus --queue-entry ");
        assert!(run[3].contains(&resume), "{run:?}");
        assert!(run[3].ends_with(close), "{run:?}");
    };
    // The idle pane the first agent stopped in runs its restore again; the
    // busy one of the second does not.
    let reused = run_in(&calls, "w1:p5").unwrap();
    resumed(&reused, &expected[0], "");
    assert!(run_in(&calls, "w1:p6").is_none(), "{calls:?}");
    // The reused pane's shell gets the caller's output setting for the agent,
    // and the resume there shows text to the person at that pane.
    assert!(reused[3].starts_with("env SHOAL_OUTPUT=text "), "{calls:?}");
    assert!(reused[3].contains(" --text resume "), "{calls:?}");
    let create = calls
        .iter()
        .find(|call| call[..2] == ["tab", "create"])
        .unwrap();
    let cwd = create.iter().position(|arg| arg == "--cwd").unwrap();
    assert_eq!(create[cwd + 1], expected[1].0);
    let label = create.iter().position(|arg| arg == "--label").unwrap();
    assert_eq!(create[label + 1], "resumed second");
    assert!(create.iter().any(|arg| arg == "--no-focus"));
    // The new tab closes with its workspace.
    let opened = run_in(&calls, "w1:p9").unwrap();
    let close = "--opened-in herdr --close-in herdr";
    resumed(&opened, &expected[1], close);
}

/// Save stopped agents of new workspaces `first` and `second` that ran in
/// Herdr panes `w1:p5` and `w1:p6`, of which only the first is back at its
/// prompt, and record the zmx sessions restores open.
fn herdr_stopped_agents(fixture: &Fixture) {
    install_fake_herdr(fixture);
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    fs::write(fixture.root.path().join("idle-panes"), "w1:p5\n").unwrap();
    for (name, pane) in [("first", "w1:p5"), ("second", "w1:p6")] {
        let workspace = fixture.add(name);
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.recovery.json")),
            format!(r#"{{"agent":"claude","mode":"worker","origin":{{"host":"herdr","place":"{pane}"}}}}"#),
        )
        .unwrap();
    }
}

/// Wait until a restore sent its command into the zmx session of each of
/// the workspaces `names`.
fn wait_for_zmx_restores(fixture: &Fixture, names: &[&str]) {
    wait_until("the restores' zmx sessions", || {
        let calls = super::zmx::zmx_calls(fixture);
        (names.iter()).all(|name| {
            let send = ["send".to_owned(), (*name).to_owned()];
            calls.iter().any(|call| call.starts_with(&send))
        })
    });
}

/// Without new tabs, an idle recorded pane is still reused, and an agent
/// whose pane is busy starts in the default host's background session.
#[test]
fn herdr_resume_without_new_tabs_reuses_idle_panes() {
    let fixture = Fixture::with_config(Some("[terminal.herdr]\nnew_tab = false\n"));
    herdr_stopped_agents(&fixture);

    let (output, transcript) = herdr_call(&fixture, &["resume", "--all", "--now"], "");

    assert!(output.status.success(), "{} {transcript}", printed(&output));
    wait_for_zmx_restores(&fixture, &["second"]);
    let calls = herdr_calls(&fixture);
    assert!(
        calls
            .iter()
            .any(|call| call[..3] == ["pane", "run", "w1:p5"]),
        "{calls:?}"
    );
    assert!(calls.iter().all(|call| call[..2] != ["tab", "create"]));
}

/// Turned off, Herdr is never called: every agent starts in the default
/// host's background session.
#[test]
fn herdr_resume_turned_off_starts_in_the_default_host() {
    let fixture = Fixture::with_config(Some("[terminal.herdr]\nenabled = false\n"));
    herdr_stopped_agents(&fixture);

    let (output, transcript) = herdr_call(&fixture, &["resume", "--all", "--now"], "");

    assert!(output.status.success(), "{} {transcript}", printed(&output));
    wait_for_zmx_restores(&fixture, &["first", "second"]);
    assert!(!fixture.root.path().join("herdr-calls").exists());
}

#[test]
fn herdr_reports_issue_lookup_errors_before_the_agent_picker() {
    let fixture = Fixture::new();
    fixture.add_github_origin();
    install_fake_herdr(&fixture);
    install_test_script(fixture.root.path().join("bin/codex"), "#!/bin/sh\nexit 0\n");
    install_test_script(
        fixture.root.path().join("bin/curl"),
        "#!/bin/sh\necho lookup-failed >&2\nexit 1\n",
    );
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\necho \"$*\" >&2\nhead -n 1\n",
    );
    let (caller, transcript) = herdr_call(&fixture, &["start", "34"], "");
    assert!(
        !caller.status.success(),
        "{} {transcript}",
        printed(&caller)
    );
    assert!(transcript.contains("Repository>"), "{transcript}");
    assert!(transcript.contains("lookup-failed"), "{transcript}");
    assert!(!transcript.contains("Agent>"), "{transcript}");
    assert!(!fixture.root.path().join("herdr-calls").exists());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
}

#[test]
fn herdr_tabs_take_the_name_of_their_space() {
    let fixture = Fixture::with_config(Some("[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n"));
    fixture.add_github_origin();
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    install_fake_herdr(&fixture);
    fixture.script_github(&github_issue(34, "Fix API timeout", "Details"));
    let url = "https://github.com/team/project/issues/34";
    for (mut args, initial, renamed, fail_rename) in [
        (
            vec![
                "start",
                "34",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "pi",
            ],
            "Fix API timeout",
            "Fix API timeout",
            false,
        ),
        (
            vec![
                "start",
                url,
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "pi",
            ],
            "Fix API timeout",
            "Fix API timeout",
            false,
        ),
        (
            vec![
                "start",
                fixture.repo.to_str().unwrap(),
                "--branch",
                "fix/short",
                "--issue",
                url,
                "--agent",
                "pi",
            ],
            "Fix API timeout",
            "Fix API timeout",
            true,
        ),
        (
            vec![
                "start",
                fixture.repo.to_str().unwrap(),
                "--branch",
                "fix/short",
                "--agent",
                "pi",
            ],
            // The branch's workspace takes the start under its name.
            "Fix API timeout",
            "Fix API timeout",
            false,
        ),
    ] {
        if !args.contains(&"--branch") {
            args.extend(["--base", "HEAD"]);
        }
        let marker = fixture.root.path().join("fail-rename");
        if fail_rename {
            fs::write(&marker, "").unwrap();
        } else if marker.exists() {
            fs::remove_file(&marker).unwrap();
        }
        let tabs = herdr_tabs(&fixture);
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{} {transcript}", printed(&caller));
        wait_for_tab(&fixture, tabs);
        let calls = herdr_changes(&fixture);
        let create = (calls.iter())
            .rfind(|call| call[..2] == ["tab", "create"])
            .unwrap();
        let label = create.iter().position(|arg| arg == "--label").unwrap();
        assert_eq!(create[label + 1], initial);
        let (worker, transcript) =
            fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
        assert!(worker.status.success(), "{} {transcript}", printed(&worker));
        assert_eq!(transcript.contains("rename-failed"), fail_rename);
        let calls = herdr_calls(&fixture);
        assert_eq!(calls.last().unwrap(), &["tab", "rename", "w1:t9", renamed]);
        if args[1] == "34" {
            fixture.ok(&["rm", "--wait", &fixture.id("Fix API timeout"), "--yes"]);
            wait_for_herdr_close(&fixture);
        }
    }
}

#[test]
fn herdr_tab_name_templates_survive_handoff() {
    let fixture = Fixture::with_config(Some(
        "[terminal.herdr]\ntab_name = 'global'\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    fixture.add_github_origin();
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[terminal.herdr]\ntab_name = 'worktree'\n",
    )
    .unwrap();
    install_fake_herdr(&fixture);
    fixture.script_github(&github_issue(
        34,
        "Literal {branch} $(touch injected)",
        "Details",
    ));
    for (issue, focus, initial, renamed) in [
        (
            Some("https://github.com/team/project/issues/34"),
            false,
            "project topic #34: Literal {branch} $(touch injected)",
            "project topic #34: Literal {branch} $(touch injected)",
        ),
        // A focused start's place, opened from here, takes the issue too.
        (
            Some("https://github.com/team/project/issues/34"),
            true,
            "project topic #34: Literal {branch} $(touch injected)",
            "project topic #34: Literal {branch} $(touch injected)",
        ),
        // Removal deleted the earlier topics, which had no commits.
        (None, false, "project topic #: ", "project topic #: "),
    ] {
        fixture.ok(&[
            "config",
            "set",
            "terminal.herdr.tab_name",
            "'{repo} {branch} #{issue_number}: {issue_title}'",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ]);
        let mut args = vec![
            "start",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "topic",
            "--base",
            "HEAD",
            "--agent",
            "pi",
        ];
        if let Some(url) = issue {
            args.extend(["--issue", url]);
        }
        if focus {
            args.push("--focus");
        }
        let tabs = herdr_tabs(&fixture);
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{} {transcript}", printed(&caller));
        wait_for_tab(&fixture, tabs);
        let calls = herdr_changes(&fixture);
        let create = (calls.iter())
            .rfind(|call| call[..2] == ["tab", "create"])
            .unwrap();
        let label = create.iter().position(|arg| arg == "--label").unwrap();
        assert_eq!(create[label + 1], initial);
        fixture.ok(&[
            "config",
            "set",
            "terminal.herdr.tab_name",
            "'changed after handoff'",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ]);
        let (worker, transcript) =
            fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
        assert!(worker.status.success(), "{} {transcript}", printed(&worker));
        let calls = herdr_calls(&fixture);
        assert_eq!(calls.last().unwrap(), &["tab", "rename", "w1:t9", renamed]);
        assert!(!fixture.root.path().join("injected").exists());
        let listed = fixture.ok(&["list"]);
        fixture.ok(&["rm", "--wait", listed[0]["id"].as_str().unwrap(), "--yes"]);
        wait_for_herdr_close(&fixture);
    }
}

/// A start of an existing workspace's agent names the place it opens after
/// the workspace's issue even once that issue is closed.
#[test]
fn a_reopened_workspaces_tab_takes_its_closed_issue() {
    let fixture = Fixture::with_config(Some(
        "[terminal.herdr]\ntab_name = '#{issue_number}: {issue_title}'\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    fixture.add_github_origin();
    install_fake_herdr(&fixture);
    let mut issue = github_issue(34, "Fix the timeout", "Details");
    fixture.script_github(&issue);
    let url = "https://github.com/team/project/issues/34";
    let repo = fixture.repo.to_str().unwrap();
    let added = fixture.ok(&["create", repo, "--branch", "topic", "--issue", url]);
    let id = added["id"].as_str().unwrap();
    issue["GET repos/team/project/issues/34"]["body"]["state"] = "closed".into();
    fixture.script_github(&issue);
    let tabs = herdr_tabs(&fixture);
    let args = ["start", "--in", id, "--agent", "pi", "--focus"];
    let (caller, transcript) = herdr_call(&fixture, &args, "");
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    wait_for_tab(&fixture, tabs);
    let calls = herdr_changes(&fixture);
    let create = (calls.iter())
        .rfind(|call| call[..2] == ["tab", "create"])
        .unwrap();
    let label = create.iter().position(|arg| arg == "--label").unwrap();
    assert_eq!(create[label + 1], "#34: Fix the timeout");
}

/// `shoal rename` run in the tab Shoal opened for `workspace`.
fn rename_in_herdr(fixture: &Fixture, workspace: &str, name: &str) -> Output {
    fixture
        .command()
        .args(["--json", "rename", workspace, name])
        .env("HERDR_ENV", "1")
        .env("HERDR_WORKSPACE_ID", "w1")
        .env("HERDR_TAB_ID", "w1:t9")
        .env("HERDR_PANE_ID", "w1:p9")
        .output()
        .unwrap()
}

#[test]
fn a_rename_renames_the_herdr_tab_shoal_opened_for_it() {
    // Without a watcher, nothing polls the tab after the fixture is gone.
    let fixture = Fixture::with_config(Some(
        "[terminal.herdr]\nclose_when_done = false\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    let args = [
        "start",
        fixture.repo.to_str().unwrap(),
        "--branch",
        "topic",
        "--base",
        "HEAD",
        "--agent",
        "pi",
    ];
    let (caller, transcript) = herdr_call(&fixture, &args, "");
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{} {transcript}", printed(&worker));

    let topic = fixture.id("topic");
    let renamed = rename_in_herdr(&fixture, &topic, "Agent tab names");
    assert!(renamed.status.success(), "{}", printed(&renamed));
    let record: Value = serde_json::from_slice(&renamed.stdout).unwrap();
    assert_eq!(record["name"], "Agent tab names");
    assert_eq!(
        herdr_changes(&fixture).last().unwrap(),
        &["tab", "rename", "w1:t9", "Agent tab names"]
    );
    assert_eq!(
        fixture.ok(&["status", &topic])["workspace"]["name"],
        "Agent tab names"
    );

    // A tab the user renamed keeps its name.
    fs::write(
        fixture.root.path().join("herdr-tab-labels"),
        serde_json::json!({"w1:t9": "notes"}).to_string(),
    )
    .unwrap();
    let before = herdr_changes(&fixture).len();
    let renamed = rename_in_herdr(&fixture, &topic, "Tab names");
    assert!(renamed.status.success(), "{}", printed(&renamed));
    assert_eq!(herdr_changes(&fixture).len(), before);

    // A tab the user closed keeps the name on the record only, though it
    // showed the label Shoal gave it.
    fs::write(
        fixture.root.path().join("herdr-tab-labels"),
        serde_json::json!({"w1:t9": "Tab names"}).to_string(),
    )
    .unwrap();
    fs::write(fixture.root.path().join("closed-tabs"), "w1:t9\n").unwrap();
    let before = herdr_changes(&fixture).len();
    let renamed = rename_in_herdr(&fixture, &topic, "Renamed later");
    assert!(renamed.status.success(), "{}", printed(&renamed));
    assert!(
        renamed.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&renamed.stderr)
    );
    assert_eq!(herdr_changes(&fixture).len(), before);

    let refused = rename_in_herdr(&fixture, &topic, "two\nlines");
    assert!(!refused.status.success());
    // Without a space, the name is refused before a picker asks for one.
    let (refused, transcript) =
        fixture.interactive_command(fixture.command().args(["rename", "   "]), "");
    assert!(!refused.status.success());
    assert!(
        transcript.contains("a space's name cannot be empty"),
        "{transcript}"
    );
}

#[test]
fn herdr_plans_name_the_launch_instead_of_its_issue() {
    let fixture = Fixture::new();
    fixture.add_github_origin();
    install_fake_herdr(&fixture);
    install_fake_agents(&fixture, &["codex"]);
    // Larger than one Linux argument may be, so it cannot ride in the tab's --env.
    let body = "x".repeat(256 * 1024);
    fixture.script_github(&github_issue(34, "Large", &body));
    let repo = fixture.repo.to_str().unwrap();
    let args = [
        "start", "34", "--repo", repo, "--agent", "codex", "--base", "HEAD",
    ];
    let (caller, transcript) = herdr_call(&fixture, &args, "");
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    let calls = herdr_calls(&fixture);
    let plan = calls[0]
        .iter()
        .find_map(|arg| arg.strip_prefix("SHOAL_PLACE_PLAN="))
        .unwrap();
    // The place reads the launch, issue included, from its queue entry.
    assert!(!plan.contains("xxxx"), "{plan}");
    let plan: Value = serde_json::from_str(plan).unwrap();
    assert!(plan["queue_entry"].is_string(), "{plan}");
    // The issue prompt lets the agent start without the user.
    assert!(calls[0].iter().any(|arg| arg == "--no-focus"));
}

#[test]
fn herdr_focus_defaults_follow_the_agent_input_and_allow_overrides() {
    for (focus, agent, flag, expected) in [
        (Some(false), None, false, "--no-focus"),
        (None, Some("pi"), false, "--focus"),
        (Some(false), Some("pi"), false, "--no-focus"),
        (Some(false), Some("pi"), true, "--focus"),
    ] {
        let mut config =
            focus.map_or_else(String::new, |focus| format!("terminal.focus = {focus}\n"));
        config.push_str("[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n");
        let fixture = Fixture::with_config(Some(&config));
        install_fake_herdr(&fixture);
        // A workspace without an agent comes from create.
        let command = if agent.is_some() { "start" } else { "create" };
        let mut args = vec![command, fixture.repo.to_str().unwrap(), "--branch", "focus"];
        if let Some(agent) = agent {
            args.extend(["--agent", agent]);
        }
        if flag {
            args.push("--focus");
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{} {transcript}", printed(&caller));
        assert!(herdr_calls(&fixture)[0].iter().any(|arg| arg == expected));
    }

    let fixture = Fixture::new();
    fixture.add_github_origin();
    install_fake_herdr(&fixture);
    // A workspace on an issue without an agent waits for the user.
    fixture.script_github(&github_issue(34, "Focus", ""));
    let (caller, transcript) = herdr_call(
        &fixture,
        &["create", "34", "--repo", fixture.repo.to_str().unwrap()],
        "",
    );
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    assert!(herdr_calls(&fixture)[0].iter().any(|arg| arg == "--focus"));
}

#[test]
fn a_json_start_opens_its_tab_only_from_a_terminal() {
    let fixture = Fixture::with_config(Some("[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n"));
    install_fake_herdr(&fixture);
    let repo = fixture.repo.to_str().unwrap();
    let args = |name| {
        [
            "--json", "start", repo, "--branch", name, "--agent", "pi", "--focus",
        ]
    };
    let refused = fixture
        .command()
        .args(args("detached"))
        .env("HERDR_ENV", "1")
        .output()
        .unwrap();
    assert!(!refused.status.success(), "{}", printed(&refused));
    assert!(!fixture.root.path().join("herdr-calls").exists());
    let (caller, transcript) = herdr_call(&fixture, &args("shown"), "");
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    let started: Value = serde_json::from_slice(&caller.stdout).unwrap();
    assert_eq!(
        (&started["space"], &started["agent"]),
        (&"shown".into(), &"pi".into()),
        "{started}"
    );
    assert!(herdr_calls(&fixture)[0].iter().any(|arg| arg == "--focus"));
    // An unattended agent's tab stays in the background: the queue opens
    // it once the agent starts, and JSON reports the recorded workspace.
    let background = [
        "--json",
        "start",
        repo,
        "--branch",
        "background",
        "--agent",
        "pi",
        "--now",
        "--",
        "task",
    ];
    let (caller, transcript) = herdr_call(&fixture, &background, "");
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    let mut reported = serde_json::Deserializer::from_slice(&caller.stdout).into_iter::<Value>();
    let started = reported.next().unwrap().unwrap();
    assert_eq!(started["name"], "background", "{started}");
    let tab = support::wait::wait_for("the background tab", || {
        herdr_calls(&fixture).get(1).cloned()
    });
    assert!(!tab.iter().any(|arg| arg == "--focus"));
}

#[test]
fn attach_previous_returns_to_the_herdr_tab_a_focused_add_opened() {
    let fixture = Fixture::with_config(Some(
        "terminal.focus = false\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    let add = |name: &str, focus: bool| {
        let mut args = vec![
            "start",
            fixture.repo.to_str().unwrap(),
            "--branch",
            name,
            "--agent",
            "pi",
        ];
        if focus {
            args.push("--focus");
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{} {transcript}", printed(&caller));
        let (worker, transcript) =
            fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
        assert!(worker.status.success(), "{} {transcript}", printed(&worker));
    };
    // A tab opened in the background shows nothing to return to.
    add("hidden", false);
    let (output, _) = herdr_call(&fixture, &["attach", "-"], "");
    assert!(!output.status.success());
    add("shown", true);
    let before = herdr_calls(&fixture).len();
    let (output, transcript) = herdr_call(&fixture, &["attach", "-"], "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(
        herdr_calls(&fixture)[before..],
        [vec!["tab", "focus", "w1:t9"]]
    );
}

#[test]
fn herdr_no_agent_opens_workspace_shell_and_honors_focus_setting() {
    let fixture = Fixture::with_config(Some("terminal.focus = false\n"));
    install_fake_herdr(&fixture);
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "create",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "herdr-shell",
        ],
        "",
    );
    assert!(caller.status.success());
    assert!(
        herdr_calls(&fixture)[0]
            .iter()
            .any(|arg| arg == "--no-focus")
    );
    let shell = fixture.root.path().join("bin/test-shell");
    install_test_script(&shell, "#!/bin/sh\npwd > \"$HOME/shell-cwd\"\n");
    let (worker, transcript) =
        fixture.interactive_command(herdr_worker_command(&fixture).env("SHELL", shell), "");
    assert!(worker.status.success(), "{} {transcript}", printed(&worker));
    let workspace = fixture.ok(&["status", &fixture.id("herdr-shell")]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("shell-cwd"))
            .unwrap()
            .trim(),
        workspace["workspace"]["path"].as_str().unwrap()
    );
    assert_eq!(herdr_changes(&fixture).len(), 3);
    fixture.ok(&["rm", "--wait", &fixture.id("herdr-shell"), "--yes"]);
    wait_for_herdr_close(&fixture);
}

#[test]
fn herdr_opt_out_and_noninteractive_calls_stay_in_place() {
    let fixture = Fixture::new();
    install_fake_herdr(&fixture);
    for (name, mode) in [("pipe", "pipe"), ("outside", "outside")] {
        let mut command = fixture.command();
        command.args(["create", fixture.repo.to_str().unwrap(), "--branch", name]);
        if mode != "outside" {
            command
                .env("HERDR_ENV", "1")
                .env("HERDR_WORKSPACE_ID", "w1");
        }
        let output = match mode {
            "outside" => fixture.interactive_command(&mut command, "").0,
            _ => command.output().unwrap(),
        };
        assert!(output.status.success(), "{}", printed(&output));
    }
    let (help, _) = herdr_call(&fixture, &["start", "--help"], "");
    assert!(help.status.success());
    assert!(!fixture.root.path().join("herdr-calls").exists());
    for (name, config) in [
        ("no-tab", "[terminal.herdr]\nnew_tab = false\n"),
        ("disabled", "[terminal.herdr]\nenabled = false\n"),
    ] {
        fs::write(fixture.repo.join(".shoal.toml"), config).unwrap();
        let (output, _) = herdr_call(
            &fixture,
            &["create", fixture.repo.to_str().unwrap(), "--branch", name],
            "",
        );
        assert!(output.status.success(), "{}", printed(&output));
        assert!(!fixture.root.path().join("herdr-calls").exists());
    }
}

#[test]
fn herdr_retains_setup_failures_and_can_disable_completion_closure() {
    let fixture = Fixture::with_config(Some(
        "[terminal.herdr]\nclose_when_done = false\n[cleanup.done]\nenabled = false\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "start",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "retain",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success());
    assert!(
        !herdr_worker(&fixture)
            .iter()
            .any(|arg| arg == "--close-when-done")
    );
    let (worker, _) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{}", printed(&worker));
    assert_eq!(herdr_changes(&fixture).len(), 3);
    fixture.ok(&["done", &fixture.id("retain")]);
    assert_eq!(herdr_changes(&fixture).len(), 3);
    // Enable closure, then fail setup before the agent can start.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "setup.command = 'fail-setup'\n[terminal.herdr]\nclose_when_done = true\n",
    )
    .unwrap();
    install_test_script(fixture.repo.join("fail-setup"), "#!/bin/sh\nexit 17\n");
    git(&fixture.repo, &["add", ".shoal.toml", "fail-setup"]);
    git_with_identity(&fixture.repo, &["commit", "-m", "setup"]);
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\ngrep 'Keep'\n",
    );
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "start",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "failed-setup",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, _) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "\n");
    assert_eq!(worker.status.code(), Some(1));
    assert_eq!(herdr_changes(&fixture).len(), 6);
}

#[test]
fn herdr_closes_when_done_cleanup_terminates_the_agent() {
    let fixture = Fixture::with_config(Some(
        "[agents.pi]\ncommand = ['sh', '-c', '\"$SHOAL_TEST_BINARY\" done --cleanup; exec sleep 600']\n",
    ));
    install_fake_herdr(&fixture);
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "start",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "done-tab",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, transcript) = fixture.interactive_command(
        herdr_worker_command(&fixture).env("SHOAL_TEST_BINARY", support::shoal_binary()),
        "",
    );
    assert_eq!(
        worker.status.code(),
        Some(143),
        "{} {transcript}",
        printed(&worker)
    );
    wait_for_herdr_close(&fixture);
    wait_until("done cleanup", || {
        fixture.ok(&["list"]).as_array().unwrap().is_empty()
    });
    // The closed tab's shell never reaches a prompt to consume its directive.
    let directive = herdr_tab_directive(&fixture);
    wait_until("directive removal", || !directive.exists());
}

#[test]
fn herdr_retains_detached_happy_exits_and_closes_on_done_after_restart() {
    let mut fixture = Fixture::with_config(Some(
        "[cleanup.done]\nenabled = false\n[agents.happy]\nenabled = true\n",
    ));
    install_fake_herdr(&fixture);
    install_fake_agents(&fixture, &["claude"]);
    install_test_script(
        fixture.root.path().join("bin/happy"),
        "#!/bin/sh\nprintf done > \"$HOME/happy-exited\"\nexit 7\n",
    );
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "start",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "happy-tab",
            "--agent",
            "happy-claude",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{} {transcript}", printed(&worker));
    wait_until("Happy execution exit", || {
        fixture.root.path().join("happy-exited").exists()
    });
    wait_until("Happy execution reporting", || {
        fixture.ok(&["status", &fixture.id("happy-tab")])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    assert_eq!(herdr_changes(&fixture).len(), 3);
    fixture.restart();
    fixture.ok(&["done", &fixture.id("happy-tab")]);
    wait_for_herdr_close(&fixture);
    assert!(fixture.ok(&["status", &fixture.id("happy-tab")])["completion"].is_object());
}

#[test]
fn herdr_tab_watchers_end_with_their_tab_whatever_the_daemon_reports() {
    let mut fixture = Fixture::new();
    install_fake_herdr(&fixture);
    let watch = |fixture: &Fixture, workspace: &str, tab: &str| {
        fixture
            .command()
            .env("PATH", fixture.interactive_path())
            .args(["internal", "herdr-watch", workspace, tab])
            .spawn()
            .unwrap()
    };
    let tab_gets = |fixture: &Fixture, tab: &str| {
        fs::read_to_string(fixture.root.path().join("herdr-gets"))
            .unwrap_or_default()
            .lines()
            .filter(|line| serde_json::from_str::<Vec<String>>(line).unwrap()[2] == tab)
            .count()
    };
    let close_tab = |fixture: &Fixture, tab: &str| {
        let closed = fixture.root.path().join("closed-tabs");
        let mut tabs = fs::read_to_string(&closed).unwrap_or_default();
        tabs.push_str(&format!("{tab}\n"));
        fs::write(closed, tabs).unwrap();
    };
    let active = fixture.add("active")["id"].as_str().unwrap().to_owned();
    let mut watchers = Vec::new();
    for (tab, daemon_running) in [("w1:t1", true), ("w1:t2", false)] {
        if !daemon_running {
            fixture.daemon.kill();
        }
        let mut watcher = watch(&fixture, &active, tab);
        wait_until("Herdr tab polling", || tab_gets(&fixture, tab) >= 2);
        assert!(watcher.try_wait().unwrap().is_none());
        // A failed tab query is not a closed tab.
        let unreachable = fixture.root.path().join("herdr-unreachable");
        fs::write(&unreachable, "").unwrap();
        let seen = tab_gets(&fixture, tab);
        wait_until("failed Herdr tab polling", || {
            tab_gets(&fixture, tab) >= seen + 2
        });
        assert!(watcher.try_wait().unwrap().is_none());
        fs::remove_file(unreachable).unwrap();
        close_tab(&fixture, tab);
        watchers.push(watcher);
    }
    for mut watcher in watchers {
        let status = wait_for("tab watcher exit", || watcher.try_wait().unwrap());
        assert!(status.success());
    }
    // Only reads: a gone tab leaves nothing to close.
    assert!(!fixture.root.path().join("herdr-calls").exists());
}

/// Wait for Shoal to close the pane it opened the tab with, which closes
/// the tab.
fn wait_for_herdr_close(fixture: &Fixture) {
    wait_until("Herdr pane closure", || herdr_closed(fixture));
}

fn herdr_closed(fixture: &Fixture) -> bool {
    herdr_calls(fixture)
        .iter()
        .any(|call| call == &["pane", "close", "w1:p9"])
}

#[test]
fn herdr_removal_and_cleanup_close_the_pane_shoal_opened_without_a_watcher() {
    let fixture = Fixture::with_config(Some(
        "[terminal.herdr]\nclose_when_done = false\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "start",
            fixture.repo.to_str().unwrap(),
            "--branch",
            "tabbed",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{} {transcript}", printed(&worker));
    let closes = |fixture: &Fixture| {
        herdr_calls(fixture)
            .iter()
            .filter(|call| call.as_slice() == ["pane", "close", "w1:p9"])
            .count()
    };
    // The daemon's records of the places Shoal created, which a background
    // sweep may retry closing at any time.
    let recorded = |fixture: &Fixture| {
        fixture.request(serde_json::json!("owned_places"))["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|place| place["origin"]["place"] == "w1:p9")
    };
    assert!(recorded(&fixture));
    // A server Shoal cannot reach keeps the record for the next sweep.
    let unreachable = fixture.root.path().join("herdr-unreachable");
    fs::write(&unreachable, "").unwrap();
    fixture.ok(&["rm", "--wait", &fixture.id("tabbed"), "--yes"]);
    assert!(closes(&fixture) >= 1);
    assert!(recorded(&fixture));
    let alerts = fixture.ok(&["alert", "list", "--unread"]);
    assert!(
        alerts
            .as_array()
            .unwrap()
            .iter()
            .any(|alert| alert["kind"] == "cleanup_failed"),
        "{alerts}"
    );
    fs::remove_file(unreachable).unwrap();
    fixture.ok(&["cleanup", "spaces"]);
    assert!(!recorded(&fixture));
    // The pane is closed and forgotten; a later sweep leaves it alone.
    let closed = closes(&fixture);
    fixture.ok(&["cleanup", "spaces"]);
    assert_eq!(closes(&fixture), closed);
}

#[test]
fn chat_rename_relabels_a_herdr_tab_that_still_shows_the_old_name() {
    let fixture = Fixture::new();
    install_fake_herdr(&fixture);
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    // The chats last while this test's process runs.
    let start = |name: &str, pane: &str, owned: bool| {
        fixture.chat_at(
            &repository,
            name,
            serde_json::json!({"host": "herdr", "place": pane, "owned": owned}),
        );
    };
    // Shoal opened `talk`'s tab under its name; `mine` runs in a tab the
    // user named.
    start("talk", "w1:p3", true);
    start("mine", "w1:p4", false);
    fs::write(
        fixture.root.path().join("herdr-tab-labels"),
        serde_json::json!({"w1:t3": "talk", "w1:t4": "notes"}).to_string(),
    )
    .unwrap();

    fixture.ok(&["rename", &fixture.id("talk"), &fixture.id("topic")]);
    fixture.ok(&["rename", &fixture.id("mine"), &fixture.id("other")]);
    let renames: Vec<_> = herdr_calls(&fixture)
        .into_iter()
        .filter(|call| call.starts_with(&["tab".to_owned(), "rename".to_owned()]))
        .collect();
    assert_eq!(renames, [["tab", "rename", "w1:t3", "topic"]]);
}

#[test]
fn attach_shows_a_herdr_tab_and_attaches_its_session_from_outside_herdr() {
    let fixture = Fixture::new();
    install_fake_herdr(&fixture);
    let home = fixture.root.path();
    let socket = "/herdr/work/herdr.sock";
    // The chat's agent runs in a Herdr pane.
    let (_agent, _) = fixture.run_chat_with("talk", &[], |command| {
        command
            .env("HERDR_ENV", "1")
            .env("HERDR_SESSION", "work")
            .env("HERDR_SOCKET_PATH", socket)
            .env("HERDR_PANE_ID", "w1:p4");
    });
    let calls_since = |count: usize| herdr_calls(&fixture)[count..].to_vec();

    // Outside Herdr, the tab is focused and a client attaches its session.
    let started = match home.join("herdr-calls").exists() {
        true => herdr_calls(&fixture).len(),
        false => 0,
    };
    let (output, transcript) = fixture.interactive(&["attach", &fixture.id("talk")], "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(
        calls_since(started),
        [
            vec!["tab", "focus", "w1:t4"],
            vec!["session", "list", "--json"],
            vec!["session", "attach", "work"],
        ]
    );

    // In a pane of the same server, focusing the tab shows it.
    let before = herdr_calls(&fixture).len();
    let mut same = fixture.command();
    same.args(["attach", &fixture.id("talk")])
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", socket)
        .env("HERDR_PANE_ID", "w1:p1");
    let (output, transcript) = fixture.interactive_command(&mut same, "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(
        calls_since(before).last().unwrap(),
        &["tab", "focus", "w1:t4"]
    );

    // A pane of another Herdr server cannot show it.
    let before = herdr_calls(&fixture).len();
    let mut other = fixture.command();
    other
        .args(["attach", &fixture.id("talk")])
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", "/herdr/other/herdr.sock")
        .env("HERDR_PANE_ID", "w1:p1");
    let (output, transcript) = fixture.interactive_command(&mut other, "");
    assert!(!output.status.success());
    assert!(
        !calls_since(before)
            .iter()
            .any(|call| call[0] == "tab" && call[1] == "focus"),
        "{transcript}"
    );

    // Disabled, Herdr is never called and the pane is no session.
    fs::remove_file(home.join("herdr-calls")).unwrap();
    let config = home.join(".config/shoal/config.toml");
    fs::write(&config, "[terminal.herdr]\nenabled = false\n").unwrap();
    let (output, _) = fixture.interactive(&["attach", &fixture.id("talk")], "");
    assert!(!output.status.success());
    assert!(!home.join("herdr-calls").exists());
    fs::remove_file(&config).unwrap();

    // A closed pane leaves nothing to attach.
    fs::write(home.join("closed-panes"), "w1:p4\n").unwrap();
    let (output, _) = fixture.interactive(&["attach", &fixture.id("talk")], "");
    assert!(!output.status.success());
}

#[test]
fn attach_previous_returns_to_a_herdr_tab_an_agent_launch_left_open() {
    let fixture = Fixture::with_config(Some("[terminal]\ndefault_host = 'herdr'\n"));
    install_fake_herdr(&fixture);
    install_fake_agents(&fixture, &["claude"]);
    fixture.add("shown");
    fixture.add("other");
    let record = |workspace: &str, place: &str| {
        let recorded = fixture.request(serde_json::json!({"record_owned_place": {
            "workspace": fixture.id(workspace),
            "origin": {"host": "herdr", "group": "w1", "place": place,
                       "data": {"HERDR_SESSION": "work"}},
        }}));
        assert!(recorded.get("error").is_none(), "{recorded}");
    };
    record("other", "w1:p4");
    let (output, transcript) = fixture.interactive(&["attach", &fixture.id("other")], "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let previous = || {
        let before = herdr_calls(&fixture).len();
        let (output, transcript) = fixture.interactive(&["attach", "-"], "");
        assert!(
            output.status.success(),
            "{}\n{transcript}",
            printed(&output)
        );
        herdr_calls(&fixture)[before..]
            .iter()
            .find(|call| call[..2] == ["tab", "focus"])
            .unwrap()[2]
            .clone()
    };
    assert_eq!(previous(), "w1:t4");
    // Outside Herdr, an agent launch opens a tab and attaches its session.
    let mut launch = fixture.command();
    launch.args([
        "start",
        "--in",
        &fixture.id("shown"),
        "--here",
        "--agent",
        "claude",
    ]);
    let (output, transcript) = fixture.terminal_command(&mut launch, "", true);
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    // The fake Herdr runs no worker to record the tab it opened.
    record("shown", "w1:p9");
    assert_eq!(previous(), "w1:t9");
}

#[test]
fn attach_previous_returns_to_a_herdr_tab_after_its_client_detaches() {
    let fixture = Fixture::new();
    install_fake_herdr(&fixture);
    fixture.add("tabbed");
    let recorded = fixture.request(serde_json::json!({"record_owned_place": {
        "workspace": fixture.id("tabbed"),
        "origin": {"host": "herdr", "group": "w1", "place": "w1:p4",
                   "data": {"HERDR_SESSION": "work"}},
    }}));
    assert!(recorded.get("error").is_none(), "{recorded}");
    let attached = || {
        herdr_calls(&fixture)
            .iter()
            .filter(|call| call[..] == ["session", "attach", "work"])
            .count()
    };
    let (output, transcript) = fixture.interactive(&["attach", &fixture.id("tabbed")], "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(attached(), 1);
    let (output, transcript) = fixture.interactive(&["attach", "-"], "");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(attached(), 2);
}

/// A focused start of an existing workspace's agent opens its tab here, and
/// the agent there starts with the task the start gave it.
#[test]
fn a_focused_worker_start_keeps_its_task_in_the_tab_it_opens() {
    let fixture = Fixture::with_config(Some(
        "[agents.pi]\ncommand = ['sh', '-c', 'printf \"%s\\0\" \"$@\" > \"$HOME/agent-args\"', 'agent', '{args}']\n",
    ));
    let workspace = fixture.add("worker");
    install_fake_herdr(&fixture);
    let id = workspace["id"].as_str().unwrap();
    let args = [
        "start", "--in", id, "--agent", "pi", "--focus", "--", "do it",
    ];
    let (caller, transcript) = herdr_call(&fixture, &args, "");
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{} {transcript}", printed(&worker));
    let told = fs::read_to_string(fixture.root.path().join("agent-args")).unwrap();
    assert!(told.split('\0').any(|arg| arg == "do it"), "{told:?}");
}

/// An agent started in a mode whose `focus` is off opens its tab in the
/// background, whatever `terminal.focus` says.
#[test]
fn an_agent_started_in_a_background_mode_opens_its_tab_unfocused() {
    let fixture = Fixture::with_config(Some(
        "terminal.focus = true\n[modes.quiet]\nkind = 'workspace'\nfocus = false\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    let repo = fixture.repo.to_str().unwrap();
    let args = [
        "start", repo, "--branch", "quiet", "--mode", "quiet", "--agent", "pi",
    ];
    let (caller, transcript) = herdr_call(&fixture, &args, "");
    assert!(caller.status.success(), "{} {transcript}", printed(&caller));
    let create = (herdr_calls(&fixture).into_iter())
        .find(|call| call[..2] == ["tab", "create"])
        .unwrap();
    assert!(create.iter().any(|arg| arg == "--no-focus"), "{create:?}");
}
