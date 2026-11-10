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
with (root / log).open('a') as calls:
    calls.write(json.dumps(args) + '\n')
if args[:2] == ['tab', 'create']:
    print(json.dumps({'result': {'tab': {'tab_id': 'w1:t9'}, 'root_pane': {'pane_id': 'w1:p9'}}}))
elif args[:2] == ['pane', 'run']:
    (root / 'herdr-worker').write_text(json.dumps(shlex.split(args[3])))
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

pub(crate) fn herdr_worker(fixture: &Fixture) -> Vec<String> {
    serde_json::from_slice(&fs::read(fixture.root.path().join("herdr-worker")).unwrap()).unwrap()
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

pub(crate) fn herdr_calls(fixture: &Fixture) -> Vec<Vec<String>> {
    fs::read_to_string(fixture.root.path().join("herdr-calls"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn herdr_handoff_preserves_choices_and_literal_arguments() {
    let fixture = Fixture::with_config(Some(
        "[agents.pi]\ncommand = ['sh', '-c', 'printf \"%s\\0\" \"$@\" > \"$HOME/agent-args\"; echo \"${SHOAL_ADD_PLAN-unset}\" > \"$HOME/agent-plan\"; exit 7', 'agent', '{args}']\n",
    ));
    install_fake_herdr(&fixture);
    let directive = fixture.root.path().join("caller-directive");
    fs::write(&directive, "unchanged").unwrap();
    let args = [
        "start",
        fixture.repo.to_str().unwrap(),
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
    assert!(caller.status.success(), "{caller:?}");
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
    assert_eq!(fs::read_to_string(directive).unwrap(), "unchanged");
    let calls = herdr_changes(&fixture);
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
            "add",
            "--host",
            "herdr",
            "--close-when-done"
        ]
    );
    let (agent, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert_eq!(agent.status.code(), Some(7), "{agent:?} {transcript}");
    // The tab's shell, not the caller's, enters the workspace after the agent.
    let tab_directive = herdr_tab_directive(&fixture);
    let workspace = fixture.ok(&["status", "herdr-agent"]);
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
            format!(r#"{{"agent":"claude","place":"{pane}"}}"#),
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
    assert!(output.status.success(), "{output:?} {transcript}");
    let calls = herdr_calls(&fixture);
    assert_eq!(calls.len(), 6, "{calls:?}");
    let resumed =
        |run: &[String], (_, id, execution): &(String, String, String), close: &[&str]| {
            let worker = run[3].split(' ').collect::<Vec<_>>();
            let resume = [
                "resume",
                id.as_str(),
                "--execution",
                execution.as_str(),
                "--focus",
            ];
            assert_eq!(
                worker[worker.len() - 5 - close.len()..],
                [&resume[..], close].concat()
            );
        };
    assert_eq!(calls[0][..4], ["pane", "process-info", "--pane", "w1:p5"]);
    assert_eq!(calls[1][..3], ["pane", "run", "w1:p5"]);
    // The reused pane keeps the watcher its tab has; the new tab closes with
    // its workspace.
    resumed(&calls[1], &expected[0], &[]);
    // The reused pane's shell gets the caller's output setting for the agent,
    // and the resume there shows text to the person at that pane.
    assert!(
        calls[1][3].starts_with("env SHOAL_OUTPUT=text "),
        "{calls:?}"
    );
    assert!(calls[1][3].contains(" --text resume "), "{calls:?}");
    assert_eq!(calls[2][..4], ["pane", "process-info", "--pane", "w1:p6"]);
    let create = &calls[3];
    assert_eq!(create[..2], ["tab", "create"]);
    let cwd = create.iter().position(|arg| arg == "--cwd").unwrap();
    assert_eq!(create[cwd + 1], expected[1].0);
    let label = create.iter().position(|arg| arg == "--label").unwrap();
    assert_eq!(create[label + 1], "resumed second");
    assert!(create.iter().any(|arg| arg == "--no-focus"));
    // The new tab's shell is read for its prompt before the run.
    assert_eq!(calls[4][..4], ["pane", "process-info", "--pane", "w1:p9"]);
    assert_eq!(calls[5][..3], ["pane", "run", "w1:p9"]);
    resumed(
        &calls[5],
        &expected[1],
        &["--opened-in", "herdr", "--close-in", "herdr"],
    );
}

#[test]
fn herdr_resume_honors_new_tab_and_enabled_settings() {
    let fixture = Fixture::with_config(Some("[terminal.herdr]\nnew_tab = false\n"));
    install_fake_herdr(&fixture);
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
            format!(r#"{{"agent":"claude","place":"{pane}"}}"#),
        )
        .unwrap();
    }
    // Without new tabs, an idle recorded pane is still reused.
    let (output, transcript) = herdr_call(&fixture, &["resume", "--all", "--now"], "");
    assert!(output.status.success(), "{output:?} {transcript}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--execution second-agent"), "{stdout}");
    let calls = herdr_calls(&fixture);
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[1][..3], ["pane", "run", "w1:p5"]);
    assert!(calls.iter().all(|call| call[..2] != ["tab", "create"]));

    // Disabled, Herdr is never called and every agent is listed.
    fs::remove_file(fixture.root.path().join("herdr-calls")).unwrap();
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        "[terminal.herdr]\nenabled = false\n",
    )
    .unwrap();
    let (output, transcript) = herdr_call(&fixture, &["resume", "--all", "--now"], "");
    assert!(output.status.success(), "{output:?} {transcript}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--execution first-agent"), "{stdout}");
    assert!(stdout.contains("--execution second-agent"), "{stdout}");
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
    assert!(!caller.status.success(), "{caller:?} {transcript}");
    assert!(transcript.contains("Repository>"), "{transcript}");
    assert!(transcript.contains("lookup-failed"), "{transcript}");
    assert!(!transcript.contains("Agent>"), "{transcript}");
    assert!(!fixture.root.path().join("herdr-calls").exists());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
}

#[test]
fn herdr_labels_name_issues_by_repository_and_number() {
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
            "project#34",
            "project#34",
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
            "project#34",
            "project#34",
            false,
        ),
        (
            vec![
                "start",
                fixture.repo.to_str().unwrap(),
                "fix/short",
                "--issue",
                url,
                "--agent",
                "pi",
            ],
            "project#34",
            "project#34",
            true,
        ),
        (
            vec![
                "start",
                fixture.repo.to_str().unwrap(),
                "--existing",
                "fix/short",
                "--agent",
                "pi",
            ],
            "fix/short",
            "fix/short",
            false,
        ),
    ] {
        if !args.contains(&"--existing") {
            args.extend(["--base", "HEAD"]);
        }
        let marker = fixture.root.path().join("fail-rename");
        if fail_rename {
            fs::write(&marker, "").unwrap();
        } else if marker.exists() {
            fs::remove_file(&marker).unwrap();
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{caller:?} {transcript}");
        let calls = herdr_changes(&fixture);
        let create = &calls[calls.len() - 2];
        let label = create.iter().position(|arg| arg == "--label").unwrap();
        assert_eq!(create[label + 1], initial);
        let (worker, transcript) =
            fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
        assert!(worker.status.success(), "{worker:?} {transcript}");
        assert_eq!(transcript.contains("rename-failed"), fail_rename);
        let calls = herdr_calls(&fixture);
        assert_eq!(calls.last().unwrap(), &["tab", "rename", "w1:t9", renamed]);
        if args[1] == "34" {
            fixture.ok(&["rm", "34-fix-api-timeout", "--yes"]);
            wait_for_herdr_close(&fixture);
        }
    }
}

#[test]
fn herdr_tab_name_templates_survive_handoff_and_use_the_allocated_branch() {
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
    git(&fixture.repo, &["branch", "topic"]);
    for (issue, initial, renamed) in [
        (
            Some("https://github.com/team/project/issues/34"),
            "project topic #34: Literal {branch} $(touch injected)",
            "project topic-2 #34: Literal {branch} $(touch injected)",
        ),
        // Removal deleted the first topic-2, which had no commits.
        (None, "project topic #: ", "project topic-2 #: "),
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
            "topic",
            "--base",
            "HEAD",
            "--agent",
            "pi",
        ];
        if let Some(url) = issue {
            args.extend(["--issue", url]);
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{caller:?} {transcript}");
        let calls = herdr_changes(&fixture);
        let create = &calls[calls.len() - 2];
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
        assert!(worker.status.success(), "{worker:?} {transcript}");
        let calls = herdr_calls(&fixture);
        assert_eq!(calls.last().unwrap(), &["tab", "rename", "w1:t9", renamed]);
        assert!(!fixture.root.path().join("injected").exists());
        fixture.ok(&["rm", "topic", "--yes"]);
        wait_for_herdr_close(&fixture);
    }
}

/// `shoal rename` run in the tab Shoal opened for `workspace`.
fn title_in_herdr(fixture: &Fixture, workspace: &str, title: &str) -> Output {
    fixture
        .command()
        .args(["--json", "rename", workspace, title])
        .env("HERDR_ENV", "1")
        .env("HERDR_WORKSPACE_ID", "w1")
        .env("HERDR_TAB_ID", "w1:t9")
        .env("HERDR_PANE_ID", "w1:p9")
        .output()
        .unwrap()
}

#[test]
fn a_workspace_title_renames_the_herdr_tab_shoal_opened_for_it() {
    // Without a watcher, nothing polls the tab after the fixture is gone.
    let fixture = Fixture::with_config(Some(
        "[terminal.herdr]\nclose_when_done = false\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    install_fake_herdr(&fixture);
    let args = [
        "start",
        fixture.repo.to_str().unwrap(),
        "topic",
        "--base",
        "HEAD",
        "--agent",
        "pi",
    ];
    let (caller, transcript) = herdr_call(&fixture, &args, "");
    assert!(caller.status.success(), "{caller:?} {transcript}");
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{worker:?} {transcript}");

    let titled = title_in_herdr(&fixture, "topic", "Agent tab names");
    assert!(titled.status.success(), "{titled:?}");
    let record: Value = serde_json::from_slice(&titled.stdout).unwrap();
    assert_eq!(record["title"], "Agent tab names");
    assert_eq!(
        herdr_changes(&fixture).last().unwrap(),
        &["tab", "rename", "w1:t9", "Agent tab names"]
    );
    assert_eq!(
        fixture.ok(&["status", "topic"])["workspace"]["title"],
        "Agent tab names"
    );

    // A tab the user renamed keeps its name.
    fs::write(
        fixture.root.path().join("herdr-tab-labels"),
        serde_json::json!({"w1:t9": "notes"}).to_string(),
    )
    .unwrap();
    let before = herdr_changes(&fixture).len();
    let titled = title_in_herdr(&fixture, "topic", "Tab names");
    assert!(titled.status.success(), "{titled:?}");
    assert_eq!(herdr_changes(&fixture).len(), before);

    // A tab the user closed keeps the title on the record only, though it
    // showed the label Shoal gave it.
    fs::write(
        fixture.root.path().join("herdr-tab-labels"),
        serde_json::json!({"w1:t9": "Tab names"}).to_string(),
    )
    .unwrap();
    fs::write(fixture.root.path().join("closed-tabs"), "w1:t9\n").unwrap();
    let before = herdr_changes(&fixture).len();
    let titled = title_in_herdr(&fixture, "topic", "Renamed later");
    assert!(titled.status.success(), "{titled:?}");
    assert!(
        titled.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&titled.stderr)
    );
    assert_eq!(herdr_changes(&fixture).len(), before);

    let refused = title_in_herdr(&fixture, "topic", "two\nlines");
    assert!(!refused.status.success());
    // Without a workspace, the title is refused before a picker asks for one.
    let (refused, transcript) =
        fixture.interactive_command(fixture.command().args(["rename", "   "]), "");
    assert!(!refused.status.success());
    assert!(transcript.contains("titles must be"), "{transcript}");
}

#[test]
fn herdr_plans_carry_issue_urls_instead_of_bodies() {
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
    assert!(caller.status.success(), "{caller:?} {transcript}");
    let calls = herdr_calls(&fixture);
    let plan = calls[0]
        .iter()
        .find_map(|arg| arg.strip_prefix("SHOAL_ADD_PLAN="))
        .unwrap();
    let plan: Value = serde_json::from_str(plan).unwrap();
    assert_eq!(plan["issue"], "https://github.com/team/project/issues/34");
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
        let mut args = vec!["start", fixture.repo.to_str().unwrap(), "focus"];
        if let Some(agent) = agent {
            args.extend(["--agent", agent]);
        }
        if flag {
            args.push("--focus");
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{caller:?} {transcript}");
        assert!(herdr_calls(&fixture)[0].iter().any(|arg| arg == expected));
    }

    let fixture = Fixture::new();
    fixture.add_github_origin();
    install_fake_herdr(&fixture);
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\nawk -F '\t' '$2 == \"No agent\" {print}'\n",
    );
    fixture.script_github(&github_issue(34, "Focus", ""));
    let (caller, transcript) = herdr_call(
        &fixture,
        &[
            "start",
            "34",
            "--repo",
            fixture.repo.to_str().unwrap(),
            "--mode",
            "worker",
        ],
        "",
    );
    assert!(caller.status.success(), "{caller:?} {transcript}");
    assert!(herdr_calls(&fixture)[0].iter().any(|arg| arg == "--focus"));
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
            name,
            "--agent",
            "pi",
        ];
        if focus {
            args.push("--focus");
        }
        let (caller, transcript) = herdr_call(&fixture, &args, "");
        assert!(caller.status.success(), "{caller:?} {transcript}");
        let (worker, transcript) =
            fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
        assert!(worker.status.success(), "{worker:?} {transcript}");
    };
    // A tab opened in the background shows nothing to return to.
    add("hidden", false);
    let (output, _) = herdr_call(&fixture, &["attach", "-"], "");
    assert!(!output.status.success());
    add("shown", true);
    let before = herdr_calls(&fixture).len();
    let (output, transcript) = herdr_call(&fixture, &["attach", "-"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
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
        &["start", fixture.repo.to_str().unwrap(), "herdr-shell"],
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
    assert!(worker.status.success(), "{worker:?} {transcript}");
    let workspace = fixture.ok(&["status", "herdr-shell"]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("shell-cwd"))
            .unwrap()
            .trim(),
        workspace["workspace"]["path"].as_str().unwrap()
    );
    assert_eq!(herdr_changes(&fixture).len(), 3);
    fixture.ok(&["rm", "herdr-shell", "--yes"]);
    wait_for_herdr_close(&fixture);
}

#[test]
fn herdr_opt_out_and_noninteractive_calls_stay_in_place() {
    let fixture = Fixture::new();
    install_fake_herdr(&fixture);
    for (name, mode) in [
        ("here", "here"),
        ("json", "json"),
        ("pipe", "pipe"),
        ("outside", "outside"),
    ] {
        let mut command = fixture.command();
        command.args(["start", fixture.repo.to_str().unwrap(), name]);
        if mode != "outside" {
            command
                .env("HERDR_ENV", "1")
                .env("HERDR_WORKSPACE_ID", "w1");
        }
        let output = match mode {
            "here" => fixture.interactive_command(command.arg("--here"), "").0,
            "json" => fixture.interactive_command(command.arg("--json"), "").0,
            "outside" => fixture.interactive_command(&mut command, "").0,
            _ => command.output().unwrap(),
        };
        assert!(output.status.success(), "{output:?}");
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
            &["start", fixture.repo.to_str().unwrap(), name],
            "",
        );
        assert!(output.status.success(), "{output:?}");
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
    assert!(worker.status.success(), "{worker:?}");
    assert_eq!(herdr_changes(&fixture).len(), 3);
    fixture.ok(&["done", "retain"]);
    assert_eq!(herdr_changes(&fixture).len(), 3);
    // Enable closure, then fail setup before the agent can start.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "setup.command = 'fail-setup'\n[terminal.herdr]\nclose_when_done = true\n",
    )
    .unwrap();
    install_test_script(fixture.repo.join("fail-setup"), "#!/bin/sh\nexit 17\n");
    git(&fixture.repo, &["add", ".shoal.toml", "fail-setup"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "setup",
        ],
    );
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\ngrep 'Keep'\n",
    );
    let (caller, _) = herdr_call(
        &fixture,
        &[
            "start",
            fixture.repo.to_str().unwrap(),
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
    assert_eq!(worker.status.code(), Some(143), "{worker:?} {transcript}");
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
            "happy-tab",
            "--agent",
            "happy-claude",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{worker:?} {transcript}");
    wait_until("Happy execution exit", || {
        fixture.root.path().join("happy-exited").exists()
    });
    wait_until("Happy execution reporting", || {
        fixture.ok(&["status", "happy-tab"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    assert_eq!(herdr_changes(&fixture).len(), 3);
    fixture.restart();
    fixture.ok(&["done", "happy-tab"]);
    wait_for_herdr_close(&fixture);
    assert!(fixture.ok(&["status", "happy-tab"])["completion"].is_object());
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
            "tabbed",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success());
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{worker:?} {transcript}");
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
    fixture.ok(&["rm", "tabbed", "--yes"]);
    assert!(closes(&fixture) >= 1);
    assert!(recorded(&fixture));
    let alerts = fixture.ok(&["alerts"]);
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
    let start = |name: &str, pane: &str| {
        let response = fixture.request(serde_json::json!({"start_chat":{
            "repository": repository["id"], "name": name, "agent": "claude",
            "pid": std::process::id(), "origin": {"host": "herdr", "place": pane}
        }}));
        assert_eq!(response["type"], "chat", "{response}");
    };
    // Shoal opened `talk`'s tab under its name; `mine` runs in a tab the
    // user named.
    start("talk", "w1:p3");
    start("mine", "w1:p4");
    fs::write(
        fixture.root.path().join("herdr-tab-labels"),
        serde_json::json!({"w1:t3": "talk", "w1:t4": "notes"}).to_string(),
    )
    .unwrap();

    fixture.ok(&["rename", "talk", "topic"]);
    fixture.ok(&["rename", "mine", "other"]);
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
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let socket = "/herdr/work/herdr.sock";
    let response = fixture.request(serde_json::json!({"start_chat":{
        "repository": repository["id"], "name": "talk", "agent": "claude",
        "pid": std::process::id(),
        "origin": {"host": "herdr", "group": "w1", "place": "w1:p4",
                   "data": {"HERDR_SESSION": "work", "HERDR_SOCKET_PATH": socket}}
    }}));
    assert_eq!(response["type"], "chat", "{response}");
    let calls_since = |count: usize| herdr_calls(&fixture)[count..].to_vec();

    // Outside Herdr, the tab is focused and a client attaches its session.
    let (output, transcript) = fixture.interactive(&["attach", "talk"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(
        calls_since(0),
        [
            vec!["tab", "focus", "w1:t4"],
            vec!["session", "list", "--json"],
            vec!["session", "attach", "work"],
        ]
    );

    // In a pane of the same server, focusing the tab shows it.
    let before = herdr_calls(&fixture).len();
    let mut same = fixture.command();
    same.args(["attach", "talk"])
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", socket)
        .env("HERDR_PANE_ID", "w1:p1");
    let (output, transcript) = fixture.interactive_command(&mut same, "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(
        calls_since(before).last().unwrap(),
        &["tab", "focus", "w1:t4"]
    );

    // A pane of another Herdr server cannot show it.
    let before = herdr_calls(&fixture).len();
    let mut other = fixture.command();
    other
        .args(["attach", "talk"])
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
    let (output, _) = fixture.interactive(&["attach", "talk"], "");
    assert!(!output.status.success());
    assert!(!home.join("herdr-calls").exists());
    fs::remove_file(&config).unwrap();

    // A closed pane leaves nothing to attach.
    fs::write(home.join("closed-panes"), "w1:p4\n").unwrap();
    let (output, _) = fixture.interactive(&["attach", "talk"], "");
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
            "workspace": workspace,
            "origin": {"host": "herdr", "group": "w1", "place": place,
                       "data": {"HERDR_SESSION": "work"}},
        }}));
        assert!(recorded.get("error").is_none(), "{recorded}");
    };
    record("other", "w1:p4");
    let (output, transcript) = fixture.interactive(&["attach", "other"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let previous = || {
        let before = herdr_calls(&fixture).len();
        let (output, transcript) = fixture.interactive(&["attach", "-"], "");
        assert!(output.status.success(), "{output:?}\n{transcript}");
        herdr_calls(&fixture)[before..]
            .iter()
            .find(|call| call[..2] == ["tab", "focus"])
            .unwrap()[2]
            .clone()
    };
    assert_eq!(previous(), "w1:t4");
    // Outside Herdr, an agent launch opens a tab and attaches its session.
    let mut launch = fixture.command();
    launch.args(["claude", "shown"]);
    let (output, transcript) = fixture.terminal_command(&mut launch, "", true);
    assert!(output.status.success(), "{output:?}\n{transcript}");
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
        "workspace": "tabbed",
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
    let (output, transcript) = fixture.interactive(&["attach", "tabbed"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), 1);
    let (output, transcript) = fixture.interactive(&["attach", "-"], "");
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert_eq!(attached(), 2);
}
