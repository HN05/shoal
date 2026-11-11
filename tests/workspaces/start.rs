//! Starting agents: `shoal start` and its modes, agent launches, trust,
//! permissions, forks, the agent queue and desktop apps.
use super::chats::{
    chat_args, chat_session_fixture, install_recording_agent, journal, save_running_chat,
};
use super::forge::{forge_origin, github_issue, install_api_curl, script_api};
use super::zmx::{created_zmx_session, wait_zmx_sessions_ended, zmx_calls};
use super::*;

#[test]
fn a_scoped_start_refuses_a_detached_agent_without_zmx_before_creating_the_child() {
    let fixture =
        Fixture::with_config(Some("[agents.droid]\ncommand = ['fake-droid', '{args}']\n"));
    let bin = fixture.root.path().join("bin");
    install_test_script_with_mode(bin.join("fake-droid"), "#!/bin/sh\nexit 0\n", 0o700);
    fixture.add("parent");
    // Outside Herdr the daemon starts the parent agent's child agent in a
    // detached zmx session.
    let output = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "parent",
            "--",
            support::shoal_binary(),
            "start",
            fixture.repo.to_str().unwrap(),
            "child",
            "--agent",
            "droid",
        ])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), fixture.system_path()),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

#[test]
fn start_refuses_an_unavailable_agent_before_creating_the_child() {
    let fixture = Fixture::new();
    fixture.add("parent");
    let output = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "child",
        "--parent",
        "parent",
        "--agent",
        "droid",
    ]);
    assert!(!output.status.success());
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

#[test]
fn start_refuses_a_fork_without_a_running_parent_agent_before_creating_the_child() {
    let fixture = Fixture::new();
    fixture.add("parent");
    let repo = fixture.repo.to_str().unwrap();
    let output = fixture.run(&["start", repo, "child", "--parent", "parent", "--fork"]);
    assert!(!output.status.success());
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

/// The parent's agent starts a child whose agent starts from a fork of its
/// session, prompted with the arguments after `--`.
#[test]
fn start_runs_the_childs_agent_from_a_fork_of_the_parents_session() {
    let fixture = Fixture::with_config(Some(
        "[machine.overload.disk]\nenabled = false\n[agents.claude]\ncommand = ['sh', '-c', 'exec sh \"$HOME/agent.sh\" \"$@\"', 'claude']\n",
    ));
    // The daemon's worker starts the child's agent in a detached session.
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    // The parent reports its session as Claude Code's hooks do and splits
    // the child; the forked agent records its arguments.
    fs::write(
        fixture.root.path().join("agent.sh"),
        format!(
            "if [ \"$1\" = --resume ]; then printf '%s\\0' \"$@\" > \"$HOME/forked.tmp\"; mv \"$HOME/forked.tmp\" \"$HOME/forked\"; exit 0; fi\n\
             printf '{{\"session_id\":\"parent-session\"}}' | '{shoal}' internal agent-state idle >/dev/null || exit 3\n\
             '{shoal}' --json start \"$(cat \"$HOME/repo-path\")\" refused --fork --agent pi 2> \"$HOME/refused.err\"\n\
             '{shoal}' --json start \"$(cat \"$HOME/repo-path\")\" child --fork -- 'Take the child part' > \"$HOME/split.json\" 2> \"$HOME/split.err\"\n",
            shoal = support::shoal_binary(),
        ),
    )
    .unwrap();
    fs::write(
        fixture.root.path().join("repo-path"),
        fixture.repo.to_str().unwrap(),
    )
    .unwrap();
    fixture.add("parent");
    let output = fixture
        .command()
        .args(["claude", "parent"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let home = fixture.root.path();
    // It reports the child, then the agent queued there.
    let started = fs::read_to_string(home.join("split.json")).unwrap();
    let queued: Value = serde_json::from_str(started.lines().last().unwrap_or_default())
        .unwrap_or_else(|error| {
            panic!(
                "{error}: {started}{}",
                fs::read_to_string(home.join("split.err")).unwrap()
            )
        });
    assert_eq!(queued["agent"], "claude", "{queued}");
    // An agent that cannot start from a fork is refused before its
    // workspace is created.
    let refused = fs::read_to_string(home.join("refused.err")).unwrap();
    assert!(refused.contains("cannot fork a session of pi"), "{refused}");
    let names = fixture.ok(&["list"]).to_string();
    assert!(!names.contains("\"refused\""), "{names}");
    let forked = home.join("forked");
    wait_until("the forked agent start", || forked.exists());
    let args = fs::read_to_string(&forked).unwrap();
    let args: Vec<_> = args.trim_end_matches('\0').split('\0').collect();
    assert_eq!(
        args[..3],
        ["--resume", "parent-session", "--fork-session"],
        "{args:?}"
    );
    assert_eq!(args.last(), Some(&"Take the child part"), "{args:?}");
}

/// An agent starts a copy of its own workspace, whose agent starts from a
/// fork of its session and works on the same files.
#[test]
fn start_runs_a_copys_agent_from_a_fork_of_the_copied_workspaces_session() {
    let fixture = Fixture::with_config(Some(
        "[machine.overload.disk]\nenabled = false\n[agents.claude]\ncommand = ['sh', '-c', 'exec sh \"$HOME/agent.sh\" \"$@\"', 'claude']\n",
    ));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    // The agent reports its session, leaves a change and copies its
    // workspace; the forked agent records its arguments.
    fs::write(
        fixture.root.path().join("agent.sh"),
        format!(
            "if [ \"$1\" = --resume ]; then printf '%s\\0' \"$@\" > \"$HOME/forked.tmp\"; mv \"$HOME/forked.tmp\" \"$HOME/forked\"; exit 0; fi\n\
             printf '{{\"session_id\":\"parent-session\"}}' | '{shoal}' internal agent-state idle >/dev/null || exit 3\n\
             echo 'half done' > untracked\n\
             '{shoal}' --json start --copy --fork -- 'Try another approach' > \"$HOME/copy.json\" 2> \"$HOME/copy.err\"\n",
            shoal = support::shoal_binary(),
        ),
    )
    .unwrap();
    fixture.add("parent");
    let output = fixture
        .command()
        .args(["claude", "parent"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let home = fixture.root.path();
    let errors = fs::read_to_string(home.join("copy.err")).unwrap();
    let copy = &fixture.ok(&["status", "parent-copy"])["workspace"];
    assert_eq!(copy["branch"], "parent-copy", "{copy} {errors}");
    assert_eq!(copy["copied_from"]["workspace"]["name"], "parent", "{copy}");
    let path = Path::new(copy["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("untracked")).unwrap(),
        "half done\n"
    );
    let forked = home.join("forked");
    wait_until("the forked agent start", || forked.exists());
    let args = fs::read_to_string(&forked).unwrap();
    let args: Vec<_> = args.trim_end_matches('\0').split('\0').collect();
    assert_eq!(args[..2], ["--resume", "parent-session"], "{args:?}");
    assert_eq!(args.last(), Some(&"Try another approach"), "{args:?}");
}

/// A child set up for an agent that its own configuration then refuses
/// stays ready, with nothing queued.
#[test]
fn a_scoped_start_reports_an_agent_refused_after_the_childs_setup() {
    let fixture = Fixture::with_config(Some(
        "[machine.overload.disk]\nenabled = false\n[agents.waiter]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    // Only the child's setup gives the agent a model it has no template for.
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\ncase \"$PWD\" in */child) printf \"[agent]\\ndefault = 'waiter'\\nmodel = 'big'\\n\" >> .shoal.toml ;; esac\n",
    );
    commit_resource_config(&fixture.repo, "setup.command = 'setup.sh'\n");
    fixture.add("parent");
    // The parent's agent starts the child's, which its preparation queues.
    let output = fixture.run(&[
        "run",
        "--workspace",
        "parent",
        "--",
        support::shoal_binary(),
        "start",
        fixture.repo.to_str().unwrap(),
        "child",
        "--agent",
        "waiter",
    ]);
    assert!(!output.status.success(), "{output:?}");
    let child = &fixture.ok(&["status", "child"])["workspace"];
    assert_eq!(child["state"], "ready", "{child}");
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));
}

/// A scoped caller's agent always starts detached, so `--here` is refused
/// before the child exists.
#[test]
fn a_scoped_start_refuses_to_run_its_agent_here() {
    let fixture = Fixture::with_config(Some("[agents.waiter]\ncommand = ['sh', '-c', 'exit 0']\n"));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    fixture.add("parent");
    let repo = fixture.repo.to_str().unwrap();
    let start = ["start", repo, "child", "--here", "--agent", "waiter"];
    let output = scoped_command(&fixture, "parent", &start);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--here"),
        "{output:?}"
    );
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

#[test]
fn add_starts_agents_only_after_creation_and_preserves_workspace_on_exit() {
    let fixture = Fixture::with_config(Some("machine.max_agents = 'none'"));
    let bin = fixture.root.path().join("add-agent-bin");
    fs::create_dir(&bin).unwrap();
    let inspection = fixture.root.path().join("agent-inspection.json");
    let directive = fixture.root.path().join("directive");
    for agent in ["codex", "claude"] {
        let stub = bin.join(agent);
        install_test_script_with_mode(
            &stub,
            r#"#!/bin/sh
"$SHOAL_TEST_BIN" --state-dir "$SHOAL_TEST_STATE" --json status "$SHOAL_TEST_NAME" > "$SHOAL_TEST_INSPECTION" || exit 99
test -f tracked || exit 98
test -z "$SHOAL_SHELL_DIRECTIVE" || exit 97
printf '%s\n' "$PWD" "$SHOAL_WORKSPACE" "$@"
exit 7
"#,
            0o700,
        );
    }
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    for (name, agent) in [("add-codex", "codex"), ("add-claude", "claude")] {
        let output = fixture
            .command()
            .args(["start", fixture.repo.to_str().unwrap(), name, "--here"])
            .args(["--agent", agent, "--", "literal spaces; $(false)"])
            .env("PATH", fixture.restricted_path(&bin))
            .env("SHOAL_TEST_BIN", support::shoal_binary())
            .env("SHOAL_TEST_STATE", fixture.root.path().join("state"))
            .env("SHOAL_TEST_NAME", name)
            .env("SHOAL_TEST_INSPECTION", &inspection)
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        let during: Value = serde_json::from_slice(&fs::read(&inspection).unwrap()).unwrap();
        assert_eq!(during["workspace"]["name"], name);
        assert_eq!(during["executions"].as_array().unwrap().len(), 1);
        let after = fixture.ok(&["status", name]);
        assert_eq!(after["executions"], serde_json::json!([]));
        let path = after["workspace"]["path"].as_str().unwrap();
        assert!(Path::new(path).join("tracked").exists());
        assert_eq!(fs::read_to_string(&directive).unwrap(), format!("{path}\n"));
        let args = format!("{name}\nliteral spaces; $(false)\n");
        assert!(
            strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n').ends_with(
                &format!("{}\n{args}", fs::canonicalize(path).unwrap().display())
            )
        );
    }

    fs::remove_file(&inspection).unwrap();
    let output = fixture
        .command()
        .args([
            "start",
            fixture.repo.to_str().unwrap(),
            "bad-base",
            "--base",
            "missing-ref",
            "--here",
            "--agent",
            "codex",
        ])
        .env("PATH", fixture.restricted_path(&bin))
        .env("SHOAL_TEST_BIN", support::shoal_binary())
        .env("SHOAL_TEST_INSPECTION", &inspection)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!inspection.exists());

    // A missing executable is refused before any worktree is created.
    fs::remove_file(bin.join("codex")).unwrap();
    fs::write(config_dir.join("config.toml"), "").unwrap();
    let add = |name: &str| {
        fixture
            .command()
            .args([
                "start",
                fixture.repo.to_str().unwrap(),
                name,
                "--here",
                "--agent",
                "codex",
            ])
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap()
    };
    assert!(!add("missing-agent").status.success());
    assert!(!fixture.run(&["status", "missing-agent"]).status.success());

    // A launch that fails only once it starts leaves the worktree for retry.
    fs::write(
        config_dir.join("config.toml"),
        "[agents.codex]\ncommand = ['{path}/missing-codex']\n",
    )
    .unwrap();
    let output = add("failed-agent");
    assert!(!output.status.success());
    let workspace = fixture.ok(&["status", "failed-agent"]);
    assert!(
        Path::new(workspace["workspace"]["path"].as_str().unwrap())
            .join("tracked")
            .exists()
    );
    assert_eq!(workspace["executions"], serde_json::json!([]));
    assert!(
        !fixture
            .run(&["start", "--", "prompt without agent"])
            .status
            .success()
    );
}

#[test]
fn claude_app_opens_a_new_session_link_for_the_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.add("claude-app");
    let path = workspace["path"].as_str().unwrap();
    let bin = fixture.root.path().join("open-bin");
    install_test_script_with_mode(
        bin.join("open"),
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\nexit 7\n",
        0o700,
    );
    let output = fixture
        .command()
        .current_dir(path)
        .args(["open", "--app", "claude"])
        .env("PATH", fixture.restricted_path(&bin))
        .output()
        .unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        let folder = path.replace('/', "%2F");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{path}\nclaude://code/new?folder={folder}\n")
        );
    } else {
        assert!(!output.status.success());
    }
    // Arguments the app cannot take are refused before a workspace is picked.
    let output = fixture
        .command()
        .args(["open", "--app", "claude", "--", "--model", "opus"])
        .env("PATH", fixture.restricted_path(&bin))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
}

#[test]
fn agent_shortcuts_forward_arguments_without_starting_real_agents() {
    let fixture = Fixture::new();
    let workspace = fixture.add("shortcut");
    let bin = fixture.root.path().join("stub-bin");
    for agent in ["claude", "codex"] {
        let path = bin.join(agent);
        install_test_script_with_mode(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$SHOAL_WORKSPACE\" \"$@\"\n",
            0o700,
        );
        let output = fixture
            .command()
            .args([agent, "shortcut", "--", "--version", "hello with spaces"])
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
            "shortcut\n--version\nhello with spaces\n"
        );
    }
    // Current-directory resolution supplies an ID internally; Claude's
    // environment still names the human workspace, as after an fzf selection.
    for target in [Some(workspace["id"].as_str().unwrap()), None] {
        let mut command = fixture.command();
        command
            .current_dir(workspace["path"].as_str().unwrap())
            .arg("claude");
        if let Some(target) = target {
            command.arg(target);
        }
        let output = command
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
            "shortcut\n"
        );
    }
}

#[test]
fn workspace_agents_get_the_agent_and_workspaces_directories_ahead_of_their_prompt() {
    let fixture = Fixture::new();
    // Every agent gets the configured agent directories.
    let agent_tmp = fixture.root.path().join("agent-tmp");
    fs::create_dir(&agent_tmp).unwrap();
    let config = fixture.root.path().join(".config/shoal/config.toml");
    let text = fs::read_to_string(&config).unwrap();
    let text = format!("agent.dirs = ['/tmp', '{}']\n{text}", agent_tmp.display());
    fs::write(&config, text).unwrap();
    assert!(fixture.run(&["daemon", "reload"]).status.success());
    let agent_tmp = format!(
        "--add-dir={}",
        fs::canonicalize(&agent_tmp).unwrap().display()
    );
    let workspace = fixture.add("directories");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let directory = fs::canonicalize(path.parent().unwrap()).unwrap();
    let directory = directory.to_str().unwrap();
    let bin = fixture.root.path().join("directory-bin");
    for agent in ["codex", "claude", "pi"] {
        let stub = bin.join(agent);
        install_test_script_with_mode(&stub, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n", 0o700);
    }
    let claude_dir = format!("--add-dir={directory}");
    // Every agent may also read `/tmp`, the default agent directory.
    let tmp_dir = format!("--add-dir={}", fs::canonicalize("/tmp").unwrap().display());
    for (agent, expected) in [
        ("claude", vec![claude_dir.as_str(), "user prompt"]),
        ("codex", vec!["--add-dir", directory, "user prompt"]),
        ("pi", vec!["user prompt"]),
    ] {
        let output = fixture
            .command()
            .args([agent, "directories", "--", "user prompt"])
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let args = strip_guidance(&stdout, '\0');
        let args: Vec<_> = args.split('\0').filter(|arg| !arg.is_empty()).collect();
        assert!(
            args.windows(expected.len())
                .any(|window| window == expected),
            "{agent}: {args:?}"
        );
        if agent == "pi" {
            assert!(!args.iter().any(|arg| arg.contains("add-dir")), "{args:?}");
        }
        if agent == "claude" {
            assert!(args.contains(&tmp_dir.as_str()), "{args:?}");
            assert!(args.contains(&agent_tmp.as_str()), "{args:?}");
        }
    }
}

#[test]
fn agents_run_with_the_requested_or_configured_model_and_effort() {
    let fixture = Fixture::new();
    fixture.add("models");
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "[agent]\ndefault = 'pi'\nmodel = 'pi-model'\n[agents.codex]\nmodel = 'gpt'\neffort = 'low'\n",
    )
    .unwrap();
    let bin = fixture.root.path().join("model-bin");
    for agent in ["codex", "claude", "pi"] {
        install_test_script_with_mode(
            bin.join(agent),
            "#!/bin/sh\nprintf '%s\\0' \"$@\" \"effort=${CLAUDE_CODE_EFFORT_LEVEL-unset}\"\n",
            0o700,
        );
    }
    let launch = |args: &[&str]| {
        fixture
            .command()
            .args(args)
            .env("PATH", fixture.restricted_path(&bin))
            .env("CLAUDE_CODE_EFFORT_LEVEL", "inherited")
            .output()
            .unwrap()
    };
    let arguments = |output: std::process::Output| {
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        stdout.split('\0').map(str::to_owned).collect::<Vec<_>>()
    };
    let contains = |args: &[String], expected: &[&str]| {
        args.windows(expected.len())
            .any(|window| window == expected)
    };
    let repo = fixture.repo.to_str().unwrap();
    // Flags override the tool's configuration, and Claude's effort variable
    // follows its flag.
    let claude = arguments(launch(&[
        "start",
        repo,
        "models-claude",
        "--here",
        "--agent",
        "claude",
        "--model",
        "opus",
        "--effort",
        "high",
    ]));
    assert!(
        contains(&claude, &["--model", "opus", "--effort", "high"]),
        "{claude:?}"
    );
    assert!(claude.contains(&"effort=high".to_owned()), "{claude:?}");
    let codex = arguments(launch(&[
        "start",
        repo,
        "models-codex",
        "--here",
        "--agent",
        "codex",
        "--effort",
        "medium",
    ]));
    assert!(
        contains(
            &codex,
            &["-m", "gpt", "-c", "model_reasoning_effort=\"medium\""]
        ),
        "{codex:?}"
    );
    // A tool without a template refuses the model `[agent]` sets for it.
    let output = launch(&["pi", "models"]);
    assert!(!output.status.success(), "{output:?}");
    // An addition refuses it before creating anything.
    let output = launch(&[
        "start",
        repo,
        "unsupported",
        "--agent",
        "pi",
        "--model",
        "m1",
    ]);
    assert!(!output.status.success(), "{output:?}");
    let workspaces = fixture.ok(&["list"]);
    assert!(
        !workspaces.to_string().contains("unsupported"),
        "{workspaces}"
    );
}

#[test]
fn worker_rules_resolve_per_launch_and_reach_native_instruction_options() {
    let fixture = Fixture::new();
    let workspace = fixture.add("instructions");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(config_dir.join("rules")).unwrap();
    fs::write(
        config_dir.join("rules/worker.md"),
        "global {workspace} {branch}",
    )
    .unwrap();
    let bin = fixture.root.path().join("template-bin");
    for agent in ["codex", "claude"] {
        let stub = bin.join(agent);
        install_test_script_with_mode(&stub, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n", 0o700);
    }
    for (index, expected) in [
        "global instructions instructions".to_owned(),
        format!("repo {}", path.display()),
        "saved \"quotes\"\n$(false) {unknown}".to_owned(),
        String::new(),
        String::new(),
    ]
    .into_iter()
    .enumerate()
    {
        if index == 1 {
            fs::create_dir_all(path.join(".shoal/rules")).unwrap();
            fs::write(path.join(".shoal/rules/worker.md"), "repo {path}").unwrap();
        }
        if index == 4 {
            fs::write(config_dir.join("rules/worker.md"), "").unwrap();
        }
        if index >= 2 {
            let local = fixture.root.path().join("local.toml");
            fs::write(
                &local,
                format!(
                    "rules.worker.text = {}",
                    toml::Value::String(expected.clone())
                ),
            )
            .unwrap();
            fixture.save_local_config(fixture.repo.to_str().unwrap(), &local);
        }
        let expected = if index == 4 {
            String::new()
        } else if index == 0 || expected.is_empty() {
            "global instructions instructions".to_owned()
        } else {
            format!("global instructions instructions\n\n{expected}")
        };
        for agent in ["codex", "claude"] {
            let output = fixture
                .command()
                .args([agent, "instructions", "--", "user prompt"])
                .env("PATH", fixture.restricted_path(&bin))
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let args = without_launch_arguments(&stdout.split('\0').collect::<Vec<_>>());
            let instructions = if agent == "claude" {
                assert_eq!(args[0], "--append-system-prompt");
                args[1].to_owned()
            } else {
                assert_eq!(args[0], "-c");
                let setting: toml::Value = toml::from_str(args[1]).unwrap();
                setting["developer_instructions"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            assert_eq!(after_guidance(&instructions), expected);
            assert_eq!(args[2], "user prompt");
        }
    }
}

#[test]
fn a_starting_subagent_learns_which_workspace_it_works_in() {
    let fixture = Fixture::new();
    let parent = fixture.add("parent");
    let split = scoped_command(&fixture, "parent", &["split", "child"]);
    assert!(split.status.success(), "{split:?}");
    let child: Value = serde_json::from_slice(&split.stdout).unwrap();
    let brief = |cwd: &Value| {
        let input = serde_json::json!({"hook_event_name": "SubagentStart", "cwd": cwd});
        let output: Value =
            serde_json::from_str(&scoped_hook(&fixture, "parent", "subagent-start", input))
                .unwrap();
        assert_eq!(
            output["hookSpecificOutput"]["hookEventName"],
            "SubagentStart"
        );
        output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // Started in a child's worktree, it works there.
    let inside = brief(&child["path"]);
    assert!(inside.contains("workspace child"), "{inside}");
    // Started in its agent's workspace, it is told that workspace.
    let beside = brief(&parent["path"]);
    assert!(beside.contains("workspace parent"), "{beside}");
    assert!(!beside.contains("workspace child"), "{beside}");
}

#[test]
fn open_hands_workspaces_to_desktop_apps_without_cli_flags_or_execution_records() {
    let fixture = Fixture::new();
    let workspace = fixture.add("desktop");
    let path = workspace["path"].as_str().unwrap();
    let bin = fixture.root.path().join("desktop-bin");
    for program in ["codex", "t3"] {
        let stub = bin.join(program);
        install_test_script_with_mode(
            &stub,
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\nexit 7\n",
            0o700,
        );
        for explicit in [true, false] {
            let mut command = fixture.command();
            command.args(["open", "--app", program]);
            if explicit {
                command.arg("desktop");
            } else {
                command.current_dir(path);
            }
            let output = command
                .args(["--", "--example", "literal spaces; $(false)"])
                .env("PATH", fixture.restricted_path(&bin))
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(7),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "{}\napp\n{path}\n--example\nliteral spaces; $(false)\n",
                    fs::canonicalize(path).unwrap().display()
                )
            );
            assert_eq!(
                fixture.ok(&["status", "desktop"])["executions"],
                serde_json::json!([])
            );
        }
    }
    fixture.add("other");
    for app in ["codex", "t3"] {
        let output = fixture
            .command()
            .args([
                "run",
                "--workspace",
                "desktop",
                "--",
                support::shoal_binary(),
                "open",
                "--app",
                app,
                "other",
            ])
            .env("PATH", fixture.restricted_path(&bin))
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    // Without --app, the only installed app opens, and several need it
    // without a terminal to pick on. An installed Claude app counts too.
    let claude = cfg!(target_os = "macos") && Path::new("/Applications/Claude.app").exists();
    let codex_only = fixture.root.path().join("codex-only-bin");
    install_test_script_with_mode(
        codex_only.join("codex"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 7\n",
        0o700,
    );
    let open = |bin: &Path| {
        fixture
            .command()
            .args(["open", "desktop"])
            .env("PATH", fixture.restricted_path(bin))
            .output()
            .unwrap()
    };
    let output = open(&codex_only);
    if !claude {
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("app\n{path}\n")
        );
    }
    let output = open(&bin);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"");
    // `app.default` chooses among them.
    let config = Path::new(path).join(".shoal.toml");
    fs::write(&config, "[app]\ndefault = 'codex'\n").unwrap();
    let output = open(&bin);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.ends_with(&format!("app\n{path}\n")), "{stdout}");
    fs::remove_file(&config).unwrap();
    // Without a Codex on PATH, opening fails instead of reaching the
    // runner's own installation.
    let output = fixture
        .command()
        .args(["open", "--app", "codex", "desktop"])
        .env("PATH", fixture.system_path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn trust_entries_follow_the_config_directories_env_gives_agents() {
    // A launch can fail after trust is written, here at its diff base.
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['claude', '{diff_base}']\n",
    ));
    let home = fixture.root.path();
    // Each workspace's agents get their own directories, from the global
    // config and from the worktree's file.
    let config = home.join(".config/shoal/config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[env]\nCODEX_HOME = '{}/codex-{{workspace}}'\n",
        home.display()
    ));
    fs::write(&config, text).unwrap();
    assert!(fixture.run(&["daemon", "reload"]).status.success());
    install_test_script(
        fixture.repo.join("pre-remove.sh"),
        "#!/bin/sh\ntest ! -f fail-removal || exit 3\n",
    );
    commit_resource_config(
        &fixture.repo,
        &format!(
            "hooks.pre_remove = 'pre-remove.sh'\n[env]\nCLAUDE_CONFIG_DIR = '{}/claude-{{workspace}}'\n",
            home.display()
        ),
    );
    fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "configured",
    ]);
    fixture.add("early");
    git(&fixture.repo, &["branch", "side"]);
    fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "late",
        "--base",
        "side",
    ]);
    git(&fixture.repo, &["branch", "-D", "side"]);
    let first = fixture.add("first");
    let first_path = Path::new(first["path"].as_str().unwrap()).to_owned();
    let second = fixture.add("second");
    let second_path = Path::new(second["path"].as_str().unwrap());
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let directory = fs::canonicalize(repository["workspaces_dir"].as_str().unwrap()).unwrap();
    let checkout = fs::canonicalize(repository["path"].as_str().unwrap()).unwrap();
    install_test_script(home.join("bin/claude"), "#!/bin/sh\nexit 0\n");

    for workspace in ["early", "first"] {
        assert!(fixture.run(&["claude", workspace]).status.success());
    }
    assert!(!fixture.run(&["claude", "late"]).status.success());
    assert!(home.join("claude-late/.claude.json").exists());
    let root: Value =
        serde_json::from_str(&fs::read_to_string(home.join("claude-first/.claude.json")).unwrap())
            .unwrap();
    for key in [&checkout, &directory] {
        assert_eq!(
            root["projects"][key.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }
    assert!(!home.join(".claude.json").exists());

    // Removing the repository clears them there too, though one workspace
    // went before and another in an earlier, failed attempt.
    for workspace in ["early", "first"] {
        let codex = home.join(format!("codex-{workspace}/config.toml"));
        fs::create_dir_all(codex.parent().unwrap()).unwrap();
        fs::write(
            &codex,
            format!(
                "[projects]\n{} = {{ trust_level = 'trusted' }}\n{} = {{ trust_level = 'trusted' }}\n'/keep' = {{ trust_level = 'trusted' }}\n",
                toml_edit::Key::new(checkout.to_str().unwrap()),
                toml_edit::Key::new(directory.to_str().unwrap()),
            ),
        )
        .unwrap();
    }
    // Removal cleans where the agents ran, not where `[env]` points now.
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("/codex-{workspace}", "/codex-changed-{workspace}"),
    )
    .unwrap();
    assert!(fixture.run(&["daemon", "reload"]).status.success());
    fixture.ok(&["rm", "--wait", "early", "--yes"]);
    fs::write(second_path.join("fail-removal"), "busy").unwrap();
    assert!(
        !fixture
            .run(&["repo", "rm", "configured", "--yes"])
            .status
            .success()
    );
    assert!(!first_path.exists());
    fs::remove_file(second_path.join("fail-removal")).unwrap();
    fixture.ok(&["repo", "rm", "configured", "--yes"]);
    for workspace in ["early", "first", "late"] {
        let claude = home.join(format!("claude-{workspace}/.claude.json"));
        let root: Value = serde_json::from_str(&fs::read_to_string(&claude).unwrap()).unwrap();
        assert_eq!(root["projects"], serde_json::json!({}), "{workspace}");
    }
    for workspace in ["early", "first"] {
        let codex = home.join(format!("codex-{workspace}/config.toml"));
        let codex: toml::Value = toml::from_str(&fs::read_to_string(&codex).unwrap()).unwrap();
        assert_eq!(
            codex["projects"],
            toml::toml! { "/keep" = { trust_level = "trusted" } }.into(),
            "{workspace}"
        );
    }
}

#[test]
fn launch_profiles_run_with_their_base_integration_and_own_environment() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let config = home.join(".config/shoal/config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(
        "[env]\nPROXY_URL = 'http://proxy'\nKEPT = 'kept'\n\
         \n[agents.claude-direct]\nbase = 'claude'\nenv = { CLAUDE_CONFIG_DIR = '~/direct' }\nenv_remove = ['PROXY_URL', 'INHERITED_TOKEN', 'WEB_PORT']\n[agents.claude-port]\nbase = 'claude'\nenv = { WEB_PORT = '1' }\n",
    );
    let chat_profile =
        "[agents.claude-chat]\nbase = 'claude-direct'\nenv = { CLAUDE_CONFIG_DIR = '~/chat' }\n";
    fs::write(&config, format!("{text}{chat_profile}")).unwrap();
    assert!(fixture.run(&["daemon", "reload"]).status.success());
    commit_resource_config(&fixture.repo, "[ports.web]\nenv = 'WEB_PORT'\n");
    fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "profiled",
    ]);
    fixture.add("profiled");
    let port = fixture.ok(&["acquire", "port", "web", "--workspace", "profiled"]);
    let reserved = format!("WEB_PORT={}", port["port"]);
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let checkout = fs::canonicalize(repository["path"].as_str().unwrap()).unwrap();
    install_test_script(
        home.join("bin/claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/claude-args\"\nenv > \"$HOME/claude-env\"\n",
    );
    let launch = |agent: &str| {
        let output = fixture
            .command()
            .args([agent, "profiled"])
            .env("INHERITED_TOKEN", "secret")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        (
            fs::read_to_string(home.join("claude-args")).unwrap(),
            fs::read_to_string(home.join("claude-env")).unwrap(),
        )
    };

    // The profile gets Claude's instructions and hooks with its own
    // environment, and trust lands in its config directory.
    let (args, env) = launch("claude-direct");
    for flag in ["--append-system-prompt\n", "--settings\n"] {
        assert!(args.contains(flag), "{args}");
    }
    let direct = home.join("direct");
    assert!(
        env.lines()
            .any(|line| line == format!("CLAUDE_CONFIG_DIR={}", direct.display())),
        "{env}"
    );
    assert!(env.lines().any(|line| line == "KEPT=kept"), "{env}");
    for removed in ["PROXY_URL=", "INHERITED_TOKEN="] {
        assert!(!env.lines().any(|line| line.starts_with(removed)), "{env}");
    }
    let root: Value =
        serde_json::from_str(&fs::read_to_string(direct.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        root["projects"][checkout.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
    assert!(!home.join(".claude.json").exists());

    // Neither removing nor setting it moves a reserved port.
    assert!(env.lines().any(|line| line == reserved), "{env}");
    let (_, env) = launch("claude-port");
    assert!(env.lines().any(|line| line == reserved), "{env}");

    // Claude itself keeps the environment it inherits.
    let (_, env) = launch("claude");
    for kept in ["PROXY_URL=http://proxy", "INHERITED_TOKEN=secret"] {
        assert!(env.lines().any(|line| line == kept), "{env}");
    }
    assert!(!env.contains("CLAUDE_CONFIG_DIR="), "{env}");

    // A chat runs with its profile's environment and trusts the checkout in
    // the profile's config directory.
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .current_dir(&fixture.repo)
            .env("INHERITED_TOKEN", "secret")
            .args(["start", "--chat", "--agent", "claude-chat", "--here"]),
        "\n",
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let env = fs::read_to_string(home.join("claude-env")).unwrap();
    let chat = home.join("chat");
    assert!(
        env.lines()
            .any(|line| line == format!("CLAUDE_CONFIG_DIR={}", chat.display())),
        "{env}"
    );
    assert!(!env.contains("INHERITED_TOKEN="), "{env}");
    let trusted = |config: &Path| {
        let root: Value = serde_json::from_str(&fs::read_to_string(config).unwrap()).unwrap();
        root["projects"]
            .as_object()
            .unwrap()
            .contains_key(checkout.to_str().unwrap())
    };
    assert!(trusted(&chat.join(".claude.json")));

    // Removing the repository cleans where each profile ran, though one of
    // them is no longer configured.
    fs::write(&config, &text).unwrap();
    assert!(fixture.run(&["daemon", "reload"]).status.success());
    fixture.ok(&["repo", "rm", "profiled", "--yes"]);
    for config in [direct.join(".claude.json"), chat.join(".claude.json")] {
        assert!(!trusted(&config), "{}", config.display());
    }
}

#[test]
fn claude_launch_trusts_checkout_and_shared_repository_directory() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let workspace = fixture.add("trusted");
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let directory = fs::canonicalize(repository["workspaces_dir"].as_str().unwrap()).unwrap();
    // Claude Code keys a worktree's trust by the repository checkout.
    let checkout = fs::canonicalize(repository["path"].as_str().unwrap()).unwrap();
    let keys = [checkout.as_path(), directory.as_path()];
    let bin = home.join("bin");
    install_test_script(bin.join("claude"), "#!/bin/sh\nexit 0\n");
    let config = home.join(".claude.json");

    // First launch creates a trusted entry even before Claude has a config.
    assert!(fixture.run(&["claude", "trusted"]).status.success());
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    for path in keys {
        assert_eq!(
            root["projects"][path.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }
    assert!(root["projects"][path.to_str().unwrap()].is_null());
    assert!(root["projects"][fixture.shoal_dir().to_str().unwrap()].is_null());

    fs::write(&config, r#"{"numStartups": 1, "projects": {}}"#).unwrap();
    assert!(fixture.run(&["claude", "trusted"]).status.success());
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(root["numStartups"], 1);
    for path in keys {
        assert_eq!(
            root["projects"][path.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }
    assert!(root["projects"][path.to_str().unwrap()].is_null());
    assert!(root["projects"][fixture.shoal_dir().to_str().unwrap()].is_null());

    // An absolute CLAUDE_CONFIG_DIR selects that directory's config instead.
    let config_dir = home.join("claude-config");
    fs::write(&config, r#"{"projects": {}}"#).unwrap();
    let output = fixture
        .command()
        .args(["claude", "trusted"])
        .env("CLAUDE_CONFIG_DIR", &config_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let overridden: Value =
        serde_json::from_str(&fs::read_to_string(config_dir.join(".claude.json")).unwrap())
            .unwrap();
    for path in keys {
        assert_eq!(
            overridden["projects"][path.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }
    assert!(overridden["projects"][path.to_str().unwrap()].is_null());
    let untouched: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(untouched["projects"], serde_json::json!({}));

    // A bad override warns and still launches.
    let output = fixture
        .command()
        .args(["claude", "trusted"])
        .env("CLAUDE_CONFIG_DIR", "relative")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn codex_launches_trust_checkout_shared_repository_directory_and_external_workspace() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    // An explicit path must not make its arbitrary parent a trust root.
    let external = home.join("external/workspace");
    let workspace = fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "trusted",
        "--path",
        external.to_str().unwrap(),
    ]);
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let directory = fs::canonicalize(repository["workspaces_dir"].as_str().unwrap()).unwrap();
    let checkout = fs::canonicalize(repository["path"].as_str().unwrap()).unwrap();
    let keys = [checkout.as_path(), path.as_path(), directory.as_path()];
    let bin = home.join("bin");
    // Inspect the config from the child so trust must precede the launch.
    install_test_script(
        bin.join("codex"),
        "#!/bin/sh\ncat \"${CODEX_HOME:-$HOME/.codex}/config.toml\"\n",
    );
    let config = home.join(".codex/config.toml");
    // The tracked agent in the current workspace, the named one, and the app.
    for args in [
        &["codex"][..],
        &["codex", "trusted"],
        &["open", "--app", "codex", "trusted"],
    ] {
        for overridden in [false, true] {
            let config_dir = home.join("custom-codex");
            let target = if overridden {
                config_dir.join("config.toml")
            } else {
                config.clone()
            };
            let mut command = fixture.command();
            command.args(args);
            if args.len() == 1 {
                command.current_dir(&path);
            }
            if overridden {
                command.env("CODEX_HOME", &config_dir);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let root: toml::Value =
                toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
            for path in keys {
                assert_eq!(
                    root["projects"][path.to_str().unwrap()]["trust_level"].as_str(),
                    Some("trusted")
                );
            }
            assert!(
                root["projects"]
                    .get(path.parent().unwrap().to_str().unwrap())
                    .is_none()
            );
            assert!(
                root["projects"]
                    .get(fixture.shoal_dir().to_str().unwrap())
                    .is_none()
            );
            assert_eq!(fs::read(&target).unwrap(), output.stdout);
            if overridden {
                assert!(!config.exists());
            }
            fs::remove_file(target).unwrap();
        }
    }

    // Malformed settings remain untouched and only produce a warning.
    fs::write(&config, "projects = []\n").unwrap();
    let output = fixture.run(&["codex", "trusted"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read_to_string(config).unwrap(), "projects = []\n");
}

#[test]
fn agent_picker_can_skip_launch_select_agents_or_cancel() {
    let fixture = Fixture::with_config(Some("[agents.none]\ncommand = ['agent-probe']\n"));
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fixture.script_github(&github_issue(298, "Pick an agent", "Issue context"));
    for (tool, script) in [
        (
            "fzf",
            r#"#!/bin/sh
cat > "$HOME/picker-input"
choice=$(cat "$HOME/choice")
[ "$choice" != cancel ] || exit 130
awk -F '\t' -v choice="$choice" '$2 == choice {print}' "$HOME/picker-input"
"#,
        ),
        (
            "codex",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/agent-args\"\n",
        ),
        (
            "agent-probe",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/agent-args\"\n",
        ),
    ] {
        let path = bin.join(tool);
        install_test_script(&path, script);
    }
    let agent_args = fixture.root.path().join("agent-args");
    for choice in ["cancel", "No agent", "codex", "none"] {
        fs::write(fixture.root.path().join("choice"), choice).unwrap();
        let (output, transcript) = fixture.interactive(
            &[
                "start",
                "https://github.com/team/project/issues/298",
                "--base",
                "HEAD",
            ],
            "",
        );
        if choice == "cancel" {
            assert_eq!(output.status.code(), Some(130), "{output:?}");
            assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
            continue;
        }
        assert!(output.status.success(), "{output:?}\n{transcript}");
        let inspection = fixture.ok(&["status", "298-pick-agent"]);
        assert_eq!(inspection["workspace"]["state"], "ready");
        assert_eq!(inspection["executions"], serde_json::json!([]));
        if choice == "No agent" {
            assert!(!agent_args.exists());
            // The shared review picker also accepts the choice without a launch.
            let (output, transcript) =
                fixture.interactive(&["review", "run", "298-pick-agent"], "");
            assert!(output.status.success(), "{output:?}\n{transcript}");
            assert!(!agent_args.exists());
        } else {
            let prompt = fs::read_to_string(&agent_args).unwrap();
            assert!(prompt.contains("Issue context"), "{prompt}");
            fs::remove_file(&agent_args).unwrap();
        }
        fixture.ok(&["rm", "--wait", "298-pick-agent", "--yes"]);
    }

    // A regular add uses the same picker when no agent was named or configured.
    fs::write(fixture.root.path().join("choice"), "codex").unwrap();
    let (output, transcript) = fixture.interactive(
        &[
            "start",
            fixture.repo.to_str().unwrap(),
            "plain",
            "--base",
            "HEAD",
        ],
        "",
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert!(agent_args.exists());
}

#[test]
fn a_picked_agent_without_its_session_host_creates_no_workspace() {
    let fixture = Fixture::new();
    install_fake_agents(&fixture, &["claude"]);
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\ntee \"$HOME/picker-input\" | head -n 1\n",
    );
    // zmx stays the default host but is missing from the terminal's PATH.
    fixture.interactive_path();
    fs::remove_file(fixture.root.path().join("runner-bin/zmx")).unwrap();
    let (output, transcript) = fixture.terminal_command(
        fixture.command().args([
            "start",
            fixture.repo.to_str().unwrap(),
            "picked",
            "--base",
            "HEAD",
        ]),
        "",
        true,
    );
    let offered = fs::read_to_string(fixture.root.path().join("picker-input")).unwrap_or_default();
    assert!(
        !output.status.success(),
        "{output:?}\n{transcript}\n{offered}"
    );
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
}

#[test]
fn cli_agent_command_defaults_can_be_replaced_at_launch() {
    let fixture = Fixture::new();
    let workspace = fixture.ok(&["start", fixture.repo.to_str().unwrap(), "configured-agent"]);
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join(".shoal.toml"),
        "[agents.claude]\ncommand = ['printf', '%s\\n', '{workspace}', '{args}', '{branch}', '{path}', 'prompt={prompt}']\n[agents.codex]\ncommand = ['printf', '%s\\n', '{args}', 'custom default', 'prompt={prompt}']\n"
    ).unwrap();
    let output = fixture.run(&["claude", "configured-agent", "--", "{path}", "two words"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
        format!(
            "configured-agent\n{{path}}\ntwo words\nconfigured-agent\n{}\nprompt=\n",
            path.display()
        )
    );
    let output = fixture.run(&[
        "run",
        "claude",
        "--workspace",
        "configured-agent",
        "--",
        "{path}",
        "two words",
    ]);
    assert!(output.status.success());
    assert_eq!(
        strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
        format!(
            "configured-agent\n{{path}}\ntwo words\nconfigured-agent\n{}\nprompt=\n",
            path.display()
        )
    );
    let output = fixture.run(&["codex", "configured-agent", "--", "--model", "example"]);
    assert!(output.status.success());
    assert_eq!(
        strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
        "--model\nexample\ncustom default\nprompt=\n"
    );
    let output = fixture.run(&[
        "run",
        "codex",
        "--workspace",
        "configured-agent",
        "--",
        "--model",
        "example",
    ]);
    assert!(output.status.success());
    assert_eq!(
        strip_shoal_arguments(&String::from_utf8(output.stdout).unwrap(), '\n'),
        "--model\nexample\ncustom default\nprompt=\n"
    );
}

#[test]
fn custom_agents_launch_with_layered_prompts_scope_and_notifications() {
    let fixture = Fixture::with_config(Some(
        "agent.default = 'helper'\nrules.worker.text = 'Follow {branch}'\n[agents.helper]\ncommand = ['missing-global-launcher']\n",
    ));
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fixture.script_github(&github_issue(37, "Literal {branch}", "$(false)"));
    install_test_script_with_mode(
        bin.join("fake-agent"),
        "#!/bin/sh\ntest -n \"$SHOAL_SCOPE_TOKEN\" || exit 99\nprintf '%s\\0' \"$@\" > \"$HOME/agent-args\"\nexit 7\n",
        0o700,
    );
    fixture.add_github_origin();
    let saved = fixture.root.path().join("saved.toml");
    fs::write(
        &saved,
        "[agents.helper]\ncommand = ['fake-agent', '--prompt={prompt}', '{args}', '{branch}']\n",
    )
    .unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    let output = fixture.run(&[
        "--json",
        "start",
        "37",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--base",
        "HEAD",
        "--here",
        "--",
        "literal {prompt}",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let workspace: Value =
        serde_json::from_slice(output.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    let branch = workspace["branch"].as_str().unwrap();
    let args = fs::read_to_string(fixture.root.path().join("agent-args")).unwrap();
    assert!(
        args.starts_with(&format!("--prompt={GUIDANCE_START}")),
        "{args}"
    );
    let args = remove_guidance(&args);
    let (prompt, rest) = args.split_once('\0').unwrap();
    assert!(
        prompt.starts_with(&format!("--prompt=Follow {branch}\n\n")),
        "{prompt}"
    );
    // The issue's text is passed as written, never as placeholders.
    for value in [
        "Literal {branch}",
        "https://github.com/team/project/issues/37",
        "$(false)",
    ] {
        assert!(prompt.contains(value), "{prompt}");
    }
    assert_eq!(rest, format!("literal {{prompt}}\0{branch}\0"));
    assert_eq!(
        fixture.ok(&["status", workspace["id"].as_str().unwrap()])["executions"],
        serde_json::json!([])
    );
    assert!(!fixture.root.path().join(".claude.json").exists());
    assert!(!fixture.root.path().join(".codex/config.toml").exists());

    let output = fixture.run(&[
        "run",
        "helper",
        "--workspace",
        workspace["id"].as_str().unwrap(),
        "--",
        "literal {prompt}",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    // An agent run outside an assignment still gets Shoal's guidance.
    let args = fs::read_to_string(fixture.root.path().join("agent-args")).unwrap();
    assert!(
        args.starts_with(&format!("--prompt={GUIDANCE_START}")),
        "{args}"
    );
    assert!(
        args.ends_with(&format!("\0literal {{prompt}}\0{branch}\0")),
        "{args}"
    );

    // Without an explicit prompt slot, context precedes literal forwarded arguments.
    fs::write(
        &saved,
        "[agents.helper]\ncommand = ['fake-agent', '{args}']\n",
    )
    .unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    let output = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "custom-add",
        "--base",
        "HEAD",
        "--here",
        "--agent",
        "helper",
        "--",
        "user message",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        remove_guidance(&fs::read_to_string(fixture.root.path().join("agent-args")).unwrap()),
        "Follow custom-add\0user message\0"
    );
    let before = fixture.ok(&["list"]);
    let unknown = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "unknown-agent",
        "--agent",
        "typo",
    ]);
    assert!(!unknown.status.success());
    assert_eq!(fixture.ok(&["list"]), before);
}

/// `--permission` gives the started agent a permission, narrower or wider
/// than its mode's, which the workspace records with the mode and its
/// status shows; an unknown key is refused before anything is created.
#[test]
fn start_gives_the_agent_permissions_and_records_them() {
    let fixture = Fixture::with_config(Some(
        "agent.default = 'helper'\n[agents.helper]\ncommand = ['fake-agent']\n",
    ));
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    install_test_script_with_mode(bin.join("fake-agent"), "#!/bin/sh\nexit 7\n", 0o700);
    let repo = fixture.repo.to_str().unwrap();
    let start = [
        "--json", "start", repo, "narrowed", "--here", "--agent", "helper",
    ];
    let given = [
        "--permission",
        "push=propose",
        "--permission",
        "merge=direct",
    ];
    let output = fixture.run(&[&start[..], &given[..]].concat());
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let status = fixture.ok(&["status", "narrowed"]);
    assert_eq!(
        status["workspace"]["narrowing"],
        serde_json::json!({"push": "propose", "merge": "direct"}),
        "{status}"
    );
    // A narrower and a wider value than the worker mode's apply as given.
    for (key, value) in [("push", "propose"), ("merge", "direct")] {
        let permission = &status["permissions"][key];
        assert_eq!(
            (permission["value"].as_str(), permission["from"].as_str()),
            (Some(value), Some("given")),
            "{status}"
        );
    }
    assert_eq!(
        status["permissions"]["issue_open"]["from"], "mode",
        "{status}"
    );

    let before = fixture.ok(&["list"]);
    let unknown = [
        "start",
        repo,
        "unknown",
        "--here",
        "--permission",
        "pushes=deny",
    ];
    let output = fixture.run(&unknown);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["list"]), before);
    // A chat takes a value wider than its mode's too.
    let chat = [
        "start", "--chat", repo, "--mode", "plan", "--agent", "helper", "--here",
    ];
    let mut command = fixture.command();
    command
        .args(chat)
        .args(["--permission", "api_writes=direct"]);
    let (output, transcript) = fixture.interactive_command(&mut command, "");
    assert_eq!(output.status.code(), Some(7), "{output:?}\n{transcript}");
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(
        chats[0]["narrowing"],
        serde_json::json!({"api_writes": "direct"}),
        "{chats}"
    );
}

/// Save a stopped agent of `waiter` that started with `push=propose` for
/// the workspace `workspace` of `fixture`, returning its record.
fn stop_narrowed_agent(fixture: &Fixture, workspace: &Value) -> PathBuf {
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    let record = state.join("stopped-agent.recovery.json");
    fs::write(
        &record,
        r#"{"agent":"waiter","narrowing":{"push":"propose"}}"#,
    )
    .unwrap();
    record
}

/// `shoal resume --permission` changes the permissions a stopped agent was
/// given, which its restore registers: a key it names takes its value, even
/// wider than the mode's, `KEY=mode` drops one, and the rest keep theirs.
/// Without it the restore keeps them all, and a refused resume changes
/// nothing.
#[test]
fn resume_changes_the_permissions_a_stopped_agent_was_given() {
    let fixture = Fixture::with_config(Some(
        "[agents.waiter]\ncommand = ['sh', '-c', 'exit 0']\nresume_command = ['sh', '-c', 'exit 0']\n",
    ));
    let workspace = fixture.add("stopped");
    let narrowing = || fixture.ok(&["status", "stopped"])["workspace"]["narrowing"].clone();
    let resume = |args: &[&str]| {
        let resume = [
            "resume",
            "stopped",
            "--execution",
            "stopped-agent",
            "--focus",
        ];
        fixture.run(&[&resume[..], args].concat())
    };
    stop_narrowed_agent(&fixture, &workspace);
    let output = resume(&[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(narrowing(), serde_json::json!({"push": "propose"}));

    let record = stop_narrowed_agent(&fixture, &workspace);
    let kept = || -> Value { serde_json::from_slice(&fs::read(&record).unwrap()).unwrap() };
    // A resume refused once the change fits, here because no host can start
    // the restore in the background, changes nothing.
    let bin = fixture.root.path().join("no-host-bin");
    fs::create_dir(&bin).unwrap();
    let output = fixture
        .command()
        .args([
            "resume",
            "stopped",
            "--now",
            "--permission",
            "api_writes=deny",
        ])
        .env("PATH", fixture.restricted_path(&bin))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(kept()["narrowing"], serde_json::json!({"push": "propose"}));
    let output = resume(&["--permission", "merge=direct"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        narrowing(),
        serde_json::json!({"push": "propose", "merge": "direct"})
    );
    stop_narrowed_agent(&fixture, &workspace);
    let output = resume(&["--permission", "push=mode"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(narrowing(), Value::Null);
}

/// The narrowing a restore the daemon starts from the queue runs under,
/// which `shoal resume` with `args` queued for an agent that started with
/// `push=propose`.
fn queued_restore_narrowing(args: &[&str]) -> Value {
    let fixture = Fixture::with_config(Some(
        "[machine.overload.disk]\nenabled = false\n[agents.waiter]\ncommand = ['sh', '-c', 'exit 0']\nresume_command = ['sh', '-c', 'trap \"exit 0\" TERM; touch restored; while :; do sleep 1; done']\n",
    ));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let workspace = fixture.add("stopped");
    stop_narrowed_agent(&fixture, &workspace);
    let queued = fixture.ok(&[&["resume", "stopped"][..], args].concat());
    assert_eq!(queued["queued"].as_array().unwrap().len(), 1, "{queued}");
    let path = Path::new(workspace["path"].as_str().unwrap());
    wait_until("the restore", || path.join("restored").exists());
    let status = fixture.ok(&["status", "stopped"]);
    fixture.ok(&["stop", "stopped"]);
    status["workspace"]["narrowing"].clone()
}

/// A restore queued without `--permission` runs under the recorded
/// narrowing.
#[test]
fn a_queued_restore_keeps_the_recorded_narrowing() {
    assert_eq!(
        queued_restore_narrowing(&[]),
        serde_json::json!({"push": "propose"})
    );
}

/// A restore the daemon starts from the queue runs with the permissions its
/// `shoal resume --permission` gave.
#[test]
fn a_queued_restore_runs_with_the_permissions_its_resume_gave() {
    assert_eq!(
        queued_restore_narrowing(&["--permission", "api_writes=deny"]),
        serde_json::json!({"push": "propose", "api_writes": "deny"})
    );
}

/// `shoal resume --permission` restores a stopped chat with the permissions
/// it gives, even wider than its mode's, which the chat records with the
/// keys it leaves out; without it the chat keeps them all.
#[test]
fn resume_changes_the_permissions_a_stopped_chat_was_given() {
    let mut fixture = chat_session_fixture();
    save_running_chat(&fixture, |fixture| {
        let output = fixture.run(&["daemon", "stop"]);
        assert!(output.status.success(), "{output:?}");
    });
    assert!(fixture.daemon.child.wait().unwrap().success());
    fixture.restart();
    let narrowing = |fixture: &Fixture| fixture.ok(&["list", "--chats"])[0]["narrowing"].clone();
    let resume = |fixture: &Fixture, args: &[&str]| {
        let mut command = fixture.command();
        command.args(["resume", "talk", "--focus"]).args(args);
        let (output, transcript) = fixture.interactive_command(&mut command, "");
        assert!(output.status.success(), "{output:?}\n{transcript}");
    };

    // The restored agent exits at once, which stops the chat again.
    resume(&fixture, &["--permission", "push=propose"]);
    assert_eq!(narrowing(&fixture), serde_json::json!({"push": "propose"}));
    let wider = serde_json::json!({"push": "propose", "api_writes": "direct"});
    resume(&fixture, &["--permission", "api_writes=direct"]);
    assert_eq!(narrowing(&fixture), wider);
    resume(&fixture, &[]);
    assert_eq!(narrowing(&fixture), wider);
}

/// A repository's own mode decides the kind of space, its rules and the
/// instructions after the subject; `--worktree` starts a chat mode in a
/// workspace; the workspace records the mode of its latest agent.
#[test]
fn start_runs_a_repositorys_own_mode_and_records_it() {
    let fixture = Fixture::with_config(Some(
        "agent.default = 'helper'\n[agents.helper]\ncommand = ['missing-global-launcher']\n",
    ));
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fixture.script_github(&github_issue(37, "Sort the queue", "Body."));
    install_test_script_with_mode(
        bin.join("fake-agent"),
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$HOME/agent-args\"\nexit 7\n",
        0o700,
    );
    fixture.add_github_origin();
    let saved = fixture.root.path().join("saved.toml");
    fs::write(
        &saved,
        "[agents.helper]\ncommand = ['fake-agent', '--prompt={prompt}']\n\
         [modes.triage]\nrules = ['triage']\ninstructions = 'Label #{issue.number}.'\n\
         [rules.triage]\ntext = 'Triage {branch}'\n",
    )
    .unwrap();
    let repo = fixture.repo.to_str().unwrap();
    fixture.save_local_config(repo, &saved);
    let start = |args: &[&str]| {
        let start = ["--json", "start", "37", "--repo", repo, "--here"];
        let output = fixture.run(&[&start[..], args].concat());
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        let workspace: Value =
            serde_json::from_slice(output.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
        let args = fs::read_to_string(fixture.root.path().join("agent-args")).unwrap();
        let status = fixture.ok(&["status", workspace["name"].as_str().unwrap()]);
        (workspace, remove_guidance(&args), status)
    };
    let (workspace, args, status) = start(&["--mode", "triage", "--base", "HEAD"]);
    let branch = workspace["branch"].as_str().unwrap();
    assert_eq!(
        args,
        format!(
            "--prompt=Triage {branch}\n\nIssue #37: Sort the queue\n\
             https://github.com/team/project/issues/37\n\nIssue description:\nBody.\n\n\
             `shoal view` shows the issue with its comments and later edits.\n\nLabel #37.\0"
        )
    );
    assert_eq!(status["workspace"]["mode"], "triage", "{status}");
    // A chat mode in the issue's workspace, reopened, is told the chat rule's
    // additions and the plan, after the same subject.
    let (_, args, status) = start(&["--mode", "plan", "--worktree"]);
    assert!(
        args.starts_with("--prompt=Issue #37: Sort the queue\n"),
        "{args}"
    );
    assert!(args.ends_with("work the plan splits off.\0"), "{args}");
    assert_eq!(status["workspace"]["mode"], "plan", "{status}");
    // A mode no config defines is refused before anything is created, also
    // when a flag decides the kind.
    let before = fixture.ok(&["list"]);
    for args in [
        &["start", "38", "--repo", repo, "--mode", "missing"][..],
        &["start", repo, "topic", "--worktree", "--mode", "missing"],
    ] {
        let output = fixture.run(args);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("no mode missing"),
            "{args:?}: {output:?}"
        );
    }
    assert_eq!(fixture.ok(&["list"]), before);
    // Outside any checkout, the issue URL names the repository whose mode
    // it is.
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args([
            "--json",
            "start",
            "--mode",
            "triage",
            "--issue",
            "https://github.com/team/project/issues/37",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("no mode"), "{output:?}");
    let reopened: Value =
        serde_json::from_slice(output.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(reopened["name"], "37-sort-queue", "{output:?}");
}

/// A chat started with `--parent` goes below that space, named by its
/// number here, and records its mode.
#[test]
fn start_puts_a_chat_below_the_parent_it_names() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let number = worker["number"].to_string();
    install_recording_agent(&fixture, "claude");
    let (output, transcript) = fixture.interactive_command(
        fixture.command().current_dir(&fixture.repo).args([
            "start", "--chat", "--parent", &number, "--agent", "claude", "--name", "talk", "--here",
        ]),
        "",
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(chats[0]["parent"]["name"], "worker", "{chats}");
    assert_eq!(chats[0]["mode"], "chat", "{chats}");
    // A parent in another repository is refused before the agent picker.
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let other = other.to_str().unwrap();
    let (output, rows) = fixture.pick(&["start", "--chat", other, "--parent", &number], "1", "");
    assert!(!output.status.success(), "{output:?}");
    assert!(rows.is_empty(), "the agent picker opened: {rows}");
}

/// Without --mode, a start on a terminal whose arguments fit either kind of
/// space offers every mode, the default first, and starts in the kind of
/// space the picked one names; one that names its repository offers only
/// the modes in a repository.
#[test]
fn start_on_a_terminal_picks_its_mode() {
    let fixture = Fixture::new();
    install_recording_agent(&fixture, "claude");
    let modes = |rows: &str| -> Vec<String> {
        (rows.lines())
            .filter_map(|row| row.split('\t').next())
            .map(str::to_owned)
            .collect()
    };
    let args = ["start", "--agent", "claude", "--here"];
    let (output, rows) = fixture.pick_in(&fixture.repo, &args, "chat", "\n");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        modes(&rows),
        ["worker", "chat", "orchestrate", "plan"],
        "{rows}"
    );
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(chats[0]["mode"], "chat", "{chats}");
    let repo = fixture.repo.to_str().unwrap();
    let (_, rows) = fixture.pick(&["start", repo, "--agent", "claude"], "none", "");
    assert_eq!(modes(&rows), ["worker", "chat", "plan"], "{rows}");
}

#[test]
fn configured_ai_tools_run_as_agents_from_their_own_command() {
    let fixture = Fixture::with_config(Some(
        "rules.worker.text = 'Follow {branch}'\n[agents.droid]\ncommand = ['fake-droid', '{args}']\n[machine.agents.notes]\nskill_dir = '~/notes'\n",
    ));
    let bin = fixture.root.path().join("bin");
    for name in ["fake-droid", "repo-droid"] {
        let path = bin.join(name);
        install_test_script_with_mode(
            &path,
            format!(
                "#!/bin/sh\ntest -n \"$SHOAL_SCOPE_TOKEN\" || exit 99\nprintf '{name}\\0' > \"$HOME/agent-args\"\nprintf '%s\\0' \"$@\" >> \"$HOME/agent-args\"\nexit 7\n"
            ),
            0o700,
        );
    }
    let workspace = fixture.add("provider");
    let args =
        || remove_guidance(&fs::read_to_string(fixture.root.path().join("agent-args")).unwrap());
    for words in [
        vec!["droid", "provider", "--", "user message"],
        vec![
            "run",
            "droid",
            "--workspace",
            "provider",
            "--",
            "user message",
        ],
    ] {
        let output = fixture.run(&words);
        assert_eq!(output.status.code(), Some(7), "{words:?}: {output:?}");
        assert_eq!(
            args(),
            format!(
                "fake-droid\0Follow {}\0user message\0",
                workspace["branch"].as_str().unwrap()
            )
        );
    }

    // A repository launcher replaces the one [ai] supplies.
    let saved = fixture.root.path().join("saved.toml");
    fs::write(&saved, "[agents.droid]\ncommand = ['repo-droid']\n").unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    let output = fixture.run(&["droid", "provider"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert!(args().starts_with("repo-droid\0Follow "), "{}", args());

    // An agent only the worktree's file defines launches from that workspace.
    fs::write(
        Path::new(workspace["path"].as_str().unwrap()).join(".shoal.toml"),
        "[agents.repo-only]\ncommand = ['repo-droid']\n",
    )
    .unwrap();
    for words in [
        vec!["repo-only", "provider"],
        vec!["run", "repo-only", "--workspace", "provider"],
    ] {
        let output = fixture.run(&words);
        assert_eq!(output.status.code(), Some(7), "{words:?}: {output:?}");
        assert!(args().starts_with("repo-droid\0Follow "), "{}", args());
    }
    // Outside every workspace it is still an agent, which needs a workspace.
    let outside = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["repo-only"])
        .output()
        .unwrap();
    assert!(!outside.status.success());

    // Built-in providers without specialized launchers place the prompt the
    // same way.
    fs::copy(bin.join("fake-droid"), bin.join("opencode")).unwrap();
    let output = fixture.run(&["opencode", "provider", "--", "user message"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        args(),
        format!(
            "fake-droid\0--prompt\0Follow {}\0user message\0",
            workspace["branch"].as_str().unwrap()
        )
    );

    // A tool with only a skill directory is not an agent.
    let output = fixture.run(&["notes", "provider"]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown command \"notes\""),
        "{output:?}"
    );

    // A provider whose launcher is missing never starts.
    fs::remove_file(bin.join("repo-droid")).unwrap();
    let output = fixture.run(&["droid", "provider"]);
    assert!(!output.status.success(), "{output:?}");
}

/// Agent arguments without the hooks that deliver agent messages and the
/// other working directories, which built-in launches add.
fn without_launch_arguments<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut remaining = Vec::new();
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        if arg.starts_with("--add-dir=") {
            continue;
        }
        match (arg, args.clone().next()) {
            ("--settings", Some(value)) if value.contains("messages --hook") => {}
            ("-c", Some(value))
                if value.starts_with("hooks.")
                    || *value == "sandbox_workspace_write.network_access=true" => {}
            ("--add-dir", Some(_)) => {}
            _ => {
                remaining.push(arg);
                continue;
            }
        }
        args.next();
    }
    remaining
}

/// Captured agent arguments, one per `separator`, without launch arguments
/// or arguments that carry only Shoal's built-in guidance.
pub(crate) fn strip_shoal_arguments(text: &str, separator: char) -> String {
    let text = strip_guidance(text, separator);
    without_launch_arguments(&text.split(separator).collect::<Vec<_>>())
        .join(&separator.to_string())
}

/// Every workspace agent's prompt starts with Shoal's guidance, which spans
/// these lines and ends with the space rules.
pub(crate) const GUIDANCE_START: &str = "You are working in Shoal workspace ";

const SPACE_RULES: &str =
    include_str!("../../crates/shoal-client/src/config/templates/space_rules.md");

/// The built-in `work` rule, which the built-in modes list right after the
/// guidance.
const WORK_RULE: &str = include_str!("../../crates/shoal-client/src/config/rules/work.md");

/// Where the guidance that starts at `start` in `text` ends, with the
/// built-in `work` rule when it follows.
pub(crate) fn guidance_end(text: &str, start: usize) -> Option<usize> {
    let rules = SPACE_RULES.trim_end();
    let end = start + text[start..].find(rules)? + rules.len();
    let work = format!("\n\n{}", WORK_RULE.trim_end());
    Some(if text[end..].starts_with(&work) {
        end + work.len()
    } else {
        end
    })
}

/// Removes each argument, with the flag that introduces it, whose whole value is
/// the guidance and the built-in `work` rule; guidance followed by additions
/// stays for [`after_guidance`].
fn strip_guidance(text: &str, separator: char) -> String {
    let wrappers = [
        (format!("--append-system-prompt{separator}"), String::new()),
        (
            format!("-c{separator}developer_instructions=\"\"\"\n"),
            "\"\"\"".to_owned(),
        ),
        (String::new(), String::new()),
    ];
    let mut text = text.to_owned();
    let mut from = 0;
    while let Some(start) = text[from..].find(GUIDANCE_START).map(|start| from + start) {
        let Some(end) = guidance_end(&text, start) else {
            break;
        };
        let (before, after) = (&text[..start], &text[end..]);
        let wrapper = wrappers.iter().find(|(prefix, suffix)| {
            before
                .strip_suffix(prefix.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.ends_with(separator))
                && after
                    .strip_prefix(suffix.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(separator))
        });
        match wrapper {
            Some((prefix, suffix)) => {
                let removed_end = (end + suffix.len() + separator.len_utf8()).min(text.len());
                text.replace_range(start - prefix.len()..removed_end, "");
                from = start - prefix.len();
            }
            None => from = end,
        }
    }
    text
}

/// The additions that follow Shoal's guidance and the built-in `work` rule in
/// a prompt.
pub(crate) fn after_guidance(prompt: &str) -> String {
    assert!(prompt.starts_with(GUIDANCE_START), "{prompt}");
    remove_guidance(prompt)
}

/// `text` with each copy of the guidance, the built-in `work` rule after it
/// and the blank line after them removed.
fn remove_guidance(text: &str) -> String {
    let mut text = text.to_owned();
    while let Some(start) = text.find(GUIDANCE_START) {
        let Some(mut end) = guidance_end(&text, start) else {
            break;
        };
        if text[end..].starts_with("\n\n") {
            end += 2;
        }
        text.replace_range(start..end, "");
    }
    text
}

#[test]
fn start_takes_the_mode_an_issues_labels_name() {
    let fixture = Fixture::with_config(Some(
        "[issue]\nplan_label = 'needs-plan'\nplanned_label = 'planned'\n",
    ));
    forge_origin(&fixture);
    install_api_curl(&fixture.root.path().join("bin"));
    for (number, title, labels) in [
        (9, "Pick a cache", vec!["needs-plan"]),
        (5, "Retry uploads", vec!["planned"]),
        (3, "Track releases", vec![]),
    ] {
        let labels: Vec<_> = (labels.iter())
            .map(|name| serde_json::json!({"name": name}))
            .collect();
        script_api(
            fixture.root.path(),
            &format!("repos/team/project/issues/{number}"),
            &serde_json::json!({"number": number, "title": title, "state": "open",
                "body": "", "labels": labels, "pull_request": null,
                "html_url": format!("https://forge.example/team/project/issues/{number}")}),
        );
    }
    install_recording_agent(&fixture, "pi");
    let started_in = || {
        let workspace = fs::read_to_string(fixture.root.path().join("chat-workspace")).unwrap();
        let chat = fs::read_to_string(fixture.root.path().join("chat-id")).unwrap();
        (!workspace.trim().is_empty(), !chat.trim().is_empty())
    };

    // An issue that needs a plan starts the plan mode's chat without asking.
    let (output, rows) = fixture.pick_in(
        &fixture.repo,
        &["start", "--issue", "9", "--agent", "pi", "--here"],
        "",
        "",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(rows.is_empty(), "{rows}");
    assert_eq!(started_in(), (false, true));
    assert!(chat_args(&fixture)[0].contains("`planned`"));

    // A planned issue starts the worker without asking.
    let (output, rows) = fixture.pick_in(
        &fixture.repo,
        &["start", "--issue", "5", "--agent", "pi", "--here"],
        "",
        "",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(rows.is_empty(), "{rows}");
    assert_eq!(started_in(), (true, false));

    // An issue with neither label asks for the mode.
    let (output, rows) = fixture.pick_in(
        &fixture.repo,
        &["start", "--issue", "3", "--agent", "pi", "--here"],
        "plan",
        "",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("worker") && rows.contains("plan"), "{rows}");
    assert_eq!(started_in(), (false, true));

    // An issue of an unregistered repository is not read in this one, so
    // the start still offers to register it.
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args(["start", "https://forge.example/team/other/issues/4"])
            .args(["--agent", "pi", "--here"])
            .current_dir(&fixture.repo),
        "n\n",
        false,
    );
    assert!(!output.status.success());
    assert!(
        transcript.contains("Register https://forge.example/team/other"),
        "{transcript}"
    );
}

#[test]
fn plan_mode_opens_a_chat_on_an_issue_that_needs_a_plan() {
    let fixture = Fixture::with_config(Some(
        "[issue]\nplan_label = 'needs-plan'\nplanned_label = 'planned'\n",
    ));
    forge_origin(&fixture);
    // The agents' directory is on the terminal's PATH.
    install_api_curl(&fixture.root.path().join("bin"));
    let issue = |number: u64, title: &str| {
        serde_json::json!({"number": number, "title": title, "state": "open",
            "body": format!("Body of {number}."), "labels": [{"name": "needs-plan"}],
            "html_url": format!("https://forge.example/team/project/issues/{number}"),
            "pull_request": null})
    };
    for (number, title) in [(12, "Clarify retries"), (9, "Pick a cache")] {
        script_api(
            fixture.root.path(),
            &format!("repos/team/project/issues/{number}"),
            &issue(number, title),
        );
    }
    script_api(
        fixture.root.path(),
        "repos/team/project/issues?type=issues&state=open&labels=needs-plan&limit=50&page=1",
        &serde_json::json!([issue(12, "Clarify retries"), issue(9, "Pick a cache")]),
    );
    let plan = |args: &[&str], agent: &str| {
        install_recording_agent(&fixture, agent);
        let (output, transcript) = fixture.interactive_command(
            fixture
                .command()
                .args(["start", "--mode", "plan", "--issue"])
                .args(args)
                .args(["--agent", agent, "--here"])
                .current_dir(&fixture.repo),
            "",
        );
        assert!(output.status.success(), "{output:?}\n{transcript}");
        chat_args(&fixture)
    };

    // The issue opens the chat with the chat rule and the planning task,
    // which swaps the configured labels.
    let args = plan(&["9"], "pi");
    assert_eq!(args.len(), 1, "{args:?}");
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    assert!(args[0].contains(checkout.to_str().unwrap()), "{}", args[0]);
    for expected in [
        "Pick a cache",
        "https://forge.example/team/project/issues/9",
        "Body of 9.",
        "`needs-plan`",
        "`planned`",
    ] {
        assert!(args[0].contains(expected), "{expected}: {}", args[0]);
    }
    // Without an issue, the open issues labelled as needing a plan are
    // offered; only the plan mode takes that, so no mode is asked for.
    install_recording_agent(&fixture, "pi");
    let (output, rows) = fixture.pick_in(
        &fixture.repo,
        &["start", "--issue", "--agent", "pi", "--here"],
        "12",
        "",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(
        rows.contains("Clarify retries") && rows.contains("Pick a cache"),
        "{rows}"
    );
    assert!(chat_args(&fixture)[0].contains("Clarify retries"));
    // Outside plan mode, --issue names one.
    let output = fixture
        .command()
        .args(["start", "--chat", "--issue", "--agent", "pi"])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");

    // An agent with an instructions template keeps them apart and starts on
    // the task as its prompt.
    // A named agent that cannot start is refused before the issue is chosen.
    let output = fixture
        .command()
        .args([
            "start",
            "--mode",
            "plan",
            "--issue",
            "--agent",
            "missing-agent",
        ])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(!output.status.success());

    let args = plan(&["https://forge.example/team/project/issues/12"], "claude");
    assert_eq!(args[0], "--append-system-prompt");
    assert!(!args[1].contains("Clarify retries"), "{}", args[1]);
    assert!(args.last().unwrap().contains("Clarify retries"), "{args:?}");
    // The chat is named after the issue without asking, beside the stopped
    // chat that planned it before, which keeps its name.
    let started = journal(&fixture)
        .into_iter()
        .rfind(|event| event["kind"] == "chat_started")
        .unwrap();
    assert_eq!(
        started["chat"]["name"],
        "repo-with---quotes----literal-plan-12-2"
    );
}

#[test]
fn add_queues_agents_past_max_agents_until_removed() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 1\n[machine.overload.disk]\nenabled = false\n[agents.waiter]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    ));
    let repo = fixture.repo.to_str().unwrap();
    let mut first = fixture
        .command()
        .args(["start", repo, "first", "--here", "--agent", "waiter"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Setup runs first, so an agent has started once its entry leaves the queue.
    let started = |name: &str| {
        fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == name && workspace["running"] == true)
            && fixture.ok(&["queue"]) == serde_json::json!([])
    };
    wait_until("the first agent start", || started("first"));

    let queued = fixture.ok(&["start", repo, "second", "--here", "--agent", "waiter"]);
    assert_eq!(queued["state"], "queued", "{queued}");
    assert_eq!(queued["position"], 1, "{queued}");
    let entries = fixture.ok(&["queue"]);
    assert_eq!(entries.as_array().unwrap().len(), 1, "{entries}");
    assert_eq!(entries[0]["name"], "second");
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    assert_eq!(
        fixture.ok(&["queue", "--repo", "other"]),
        serde_json::json!([])
    );
    // A queued entry creates nothing until it starts.
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);

    // Entries move to a position and leave the queue when cancelled.
    fixture.ok(&["start", repo, "third", "--here", "--agent", "waiter"]);
    let moved = fixture.ok(&["queue", "move", "third", "1"]);
    assert_eq!(moved["entry"]["position"], 1, "{moved}");
    assert_eq!(moved["stopped"], false, "{moved}");
    let names = |entries: serde_json::Value| -> Vec<String> {
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names(fixture.ok(&["queue"])), ["third", "second"]);
    // A target that names nothing fails before a picker asks for its place.
    let missing = fixture.run(&["queue", "move", "missing"]);
    assert!(!missing.status.success());
    // So does a workspace whose agent cannot restore in place.
    let unrestorable = fixture.run(&["queue", "move", "first"]);
    assert!(!unrestorable.status.success());
    let cancelled = fixture.ok(&["queue", "cancel", "third"]);
    assert_eq!(cancelled["name"], "third", "{cancelled}");
    assert_eq!(names(fixture.ok(&["queue"])), ["second"]);
    // So does one stopped, having nothing to save.
    fixture.ok(&["start", repo, "fourth", "--here", "--agent", "waiter"]);
    let stopped = fixture.ok(&["stop", "fourth"]);
    assert_eq!(stopped["name"], "fourth", "{stopped}");
    assert_eq!(names(fixture.ok(&["queue"])), ["second"]);

    let removed = fixture.ok(&["rm", "--wait", "second"]);
    assert_eq!(removed["name"], "second", "{removed}");
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));

    // Without a terminal nothing shows a focused launch, so it is refused,
    // while one started now skips the queue.
    let refused = fixture.run(&["start", repo, "focused", "--agent", "waiter", "--focus"]);
    assert!(!refused.status.success(), "{refused:?}");
    let mut focused = fixture
        .command()
        .args([
            "start", repo, "focused", "--here", "--agent", "waiter", "--now",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the focused agent start", || started("focused"));
    fixture.ok(&["stop", "focused"]);
    focused.wait().unwrap();
    fixture.ok(&["rm", "--wait", "focused", "--yes"]);

    // A queued agent named like a workspace is removed only by its ID.
    let shared = fixture.ok(&["start", repo, "first", "--here", "--agent", "waiter"]);
    assert_eq!(shared["state"], "queued", "{shared}");
    let ambiguous = fixture.run(&["rm", "--wait", "first", "--yes"]);
    assert!(!ambiguous.status.success());
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
    fixture.ok(&["rm", "--wait", shared["id"].as_str().unwrap()]);
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));
    fixture.ok(&["stop", "first"]);
    first.wait().unwrap();
}

/// `queue start --attach` starts a waiting agent and attaches this terminal
/// to the session the daemon starts it in.
#[test]
fn queue_start_attaches_to_the_started_agents_session() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 1\n[machine.overload.disk]\nenabled = false\n[agents.waiter]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n[agents.missing]\ncommand = ['shoal-test-missing-agent']\n",
    ));
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    // zmx lists the sessions the daemon opens.
    fs::write(home.join("zmx-keep-sessions"), "").unwrap();
    let repo = fixture.repo.to_str().unwrap();
    let mut first = fixture
        .command()
        .args(["start", repo, "first", "--here", "--agent", "waiter"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the first agent start", || {
        fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "first" && workspace["running"] == true)
            && fixture.ok(&["queue"]) == serde_json::json!([])
    });
    let output = fixture
        .command()
        .args(["--json", "start", repo, "second", "--agent", "waiter"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let queued = &fixture.ok(&["queue"])[0];
    assert_eq!(queued["name"], "second");
    // A session the workspace already has, such as a shell, is not the agent's.
    let workspace = queued["opens"].as_str().unwrap();
    let recorded = fixture.request(serde_json::json!({"record_owned_place": {
        "workspace": workspace,
        "origin": {"host": "zmx", "place": "second-shell"},
    }}));
    assert!(recorded.get("error").is_none(), "{recorded}");
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(home.join("zmx-sessions"))
        .unwrap()
        .write_all(created_zmx_session(&fixture, "second-shell", workspace).as_bytes())
        .unwrap();

    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args(["queue", "start", "--attach", "second"]),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let opened = zmx_calls(&fixture)
        .into_iter()
        .find(|call| call[0] == "run")
        .unwrap();
    let attached = zmx_calls(&fixture)
        .into_iter()
        .rfind(|call| call[0] == "attach")
        .unwrap();
    assert_eq!(attached, ["attach", opened[1].as_str()]);

    // A start that fails ends the wait at once: this one's agent is gone
    // by the time the queue starts it.
    let missing = home.join("bin/shoal-test-missing-agent");
    install_test_script(&missing, "#!/bin/sh\n");
    let output = fixture
        .command()
        .args(["--json", "start", repo, "third", "--agent", "missing"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::remove_file(&missing).unwrap();
    let (output, transcript) = fixture.terminal_command(
        fixture
            .command()
            .args(["queue", "start", "--attach", "third"]),
        "",
        true,
    );
    assert!(!output.status.success(), "{output:?}\n{transcript}");
    assert!(transcript.contains("did not start"), "{transcript}");

    fixture.ok(&["stop", "second"]);
    fixture.ok(&["stop", "first"]);
    first.wait().unwrap();
    wait_zmx_sessions_ended(home);
}

/// A queued agent starts from the daemon, which gives it the output setting
/// of the caller that queued it rather than its own.
#[test]
fn a_queued_agent_keeps_its_callers_output_setting() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 1\n[machine.overload.disk]\nenabled = false\n[agents.waiter]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n[agents.recorder]\ncommand = ['sh', '-c', 'printf %s \"${SHOAL_OUTPUT-unset}\" > \"$HOME/queued-output\"; trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    ));
    // The daemon's worker starts the queued agent in a detached session.
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let repo = fixture.repo.to_str().unwrap();
    let mut first = fixture
        .command()
        .args(["start", repo, "first", "--here", "--agent", "waiter"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the first agent start", || {
        fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "first" && workspace["running"] == true)
            && fixture.ok(&["queue"]) == serde_json::json!([])
    });
    let output = fixture
        .command()
        .env("SHOAL_OUTPUT", "text")
        .args(["--json", "start", repo, "second", "--agent", "recorder"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // Without a terminal it creates the workspace, then reports the agent
    // its preparation queued there.
    let stdout = String::from_utf8(output.stdout).unwrap();
    let queued: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(queued["state"], "queued", "{queued}");
    // Removal frees the slot, which a stopped agent keeps for its resume.
    fixture.ok(&["stop", "first"]);
    first.wait().unwrap();
    fixture.ok(&["rm", "--wait", "first", "--yes"]);
    let recorded = fixture.root.path().join("queued-output");
    wait_until("the queued agent start", || recorded.exists());
    wait_until("the queued agent's record", || {
        !fs::read_to_string(&recorded).unwrap().is_empty()
    });
    assert_eq!(fs::read_to_string(&recorded).unwrap(), "text");
    fixture.ok(&["stop", "second"]);
}

/// A start without a terminal, as from another agent's tool call run in a
/// zmx session, returns once setup ends and reports the agent it queued,
/// which the daemon starts in a session of its own that outlives the caller.
#[test]
fn a_start_without_a_terminal_leaves_its_agent_to_the_daemon() {
    let fixture = Fixture::with_config(Some(&format!(
        "{UNLIMITED_AGENTS}[agents.waiter]\ncommand = ['sh', '-c', 'touch \"$HOME/waiter-started\"; trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    )));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let repo = fixture.repo.to_str().unwrap();
    let output = fixture
        .command()
        .args(["--json", "start", repo, "detached", "--agent", "waiter"])
        .env("ZMX_SESSION", "outer")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let reported: Vec<Value> = (stdout.lines())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(reported[0]["name"], "detached", "{stdout}");
    let entry = reported.last().unwrap();
    assert_eq!(entry["agent"], "waiter", "{stdout}");
    // The caller's own session stays its own, since the agent left it.
    let labelled = zmx_calls(&fixture);
    assert!(
        !(labelled.iter()).any(|call| call.starts_with(&["set".into(), "outer".into()])),
        "{labelled:?}"
    );
    // The caller has exited; the agent runs on in the daemon's session.
    let started = fixture.root.path().join("waiter-started");
    wait_until("the detached agent start", || started.exists());
    wait_until("the detached agent's execution", || {
        fixture.ok(&["status", "detached"])["executions"]
            .as_array()
            .is_some_and(|executions| !executions.is_empty())
    });
    fixture.ok(&["stop", "detached"]);
}

/// Without a terminal, as from another agent's tool call, `shoal resume
/// --now` has the daemon start each restore at once, whatever the limit,
/// instead of naming commands to run them here, and so does resuming a last
/// session by itself.
#[test]
fn resume_now_without_a_terminal_has_the_daemon_start_its_restores() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 1\n[machine.overload.disk]\nenabled = false\n[agents.waiter]\ncommand = ['sh', '-c', 'exit 0']\nresume_command = ['sh', '-c', 'trap \"exit 0\" TERM; touch restored; while :; do sleep 1; done']\n",
    ));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let mut paths = Vec::new();
    for (name, record) in [("first", "recovery"), ("second", "last-session")] {
        let workspace = fixture.add(name);
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{name}-agent.{record}.json")),
            r#"{"agent":"waiter"}"#,
        )
        .unwrap();
        paths.push(PathBuf::from(workspace["path"].as_str().unwrap()));
    }
    // A last session never waits for a slot, so it needs no --now.
    for (args, path) in [&["first", "--now"][..], &["second"]].iter().zip(&paths) {
        let resumed = fixture.ok(&[&["resume"][..], args].concat());
        let queued = resumed["queued"].as_array().unwrap();
        assert_eq!(queued.len(), 1, "{resumed}");
        assert_eq!(queued[0]["state"], "starting", "{resumed}");
        wait_until("the restore", || path.join("restored").exists());
    }
    fixture.ok(&["stop", "first"]);
    fixture.ok(&["stop", "second"]);
}

/// Without a terminal, a resume the daemon would start from the background
/// is refused before anything is queued when no host can take it, naming
/// the restore in place.
#[test]
fn resume_without_a_terminal_or_host_is_refused_before_queuing() {
    let fixture = Fixture::with_config(Some("[agents.waiter]\ncommand = ['sh', '-c', 'exit 0']\n"));
    let workspace = fixture.add("stopped");
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    let record = state.join("stopped-agent.recovery.json");
    fs::write(&record, r#"{"agent":"waiter"}"#).unwrap();
    let bin = fixture.root.path().join("no-host-bin");
    fs::create_dir(&bin).unwrap();
    let output = fixture
        .command()
        .args(["resume", "stopped", "--now"])
        .env("PATH", fixture.restricted_path(&bin))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--focus"),
        "{output:?}"
    );
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));
    assert!(record.exists());
}

/// `shoal resume` queues restores, which the daemon starts in detached
/// sessions as `max_agents` allows, leaving out done workspaces.
#[test]
fn resume_queues_restores_that_start_as_agent_slots_free() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 1\n[machine.overload.disk]\nenabled = false\n[agents.waiter]\ncommand = ['sh', '-c', 'exit 0']\nresume_command = ['sh', '-c', 'trap \"exit 0\" TERM; touch restored; while :; do sleep 1; done']\n",
    ));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let mut paths = Vec::new();
    for name in ["first", "second", "third", "finished"] {
        let workspace = fixture.add(name);
        let state = fixture
            .root
            .path()
            .join("state/workspaces")
            .join(workspace["id"].as_str().unwrap());
        fs::create_dir_all(&state).unwrap();
        // The third agent is a last session, which `--all` queues too.
        let kind = if name == "third" {
            "last-session"
        } else {
            "recovery"
        };
        fs::write(
            state.join(format!("{name}-agent.{kind}.json")),
            r#"{"agent":"waiter"}"#,
        )
        .unwrap();
        paths.push(PathBuf::from(workspace["path"].as_str().unwrap()));
    }
    fixture.ok(&["done", "finished"]);

    let queued = fixture.ok(&["resume", "--all"]);
    let names: Vec<_> = queued["queued"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["first", "second", "third"], "{queued}");
    wait_until("the first restore", || paths[0].join("restored").exists());
    // The restored agent takes the only slot, so the others wait.
    let waiting = fixture.ok(&["queue"]);
    assert_eq!(waiting.as_array().unwrap().len(), 2, "{waiting}");
    assert_eq!(waiting[0]["name"], "second", "{waiting}");
    assert_eq!(waiting[0]["state"], "queued", "{waiting}");
    let sent = zmx_calls(&fixture)
        .into_iter()
        .find(|call| call.starts_with(&["send".into(), "first".into()]))
        .unwrap();
    assert!(
        sent[2].contains(" --execution first-agent --focus --queue-entry "),
        "{sent:?}"
    );

    fixture.ok(&["stop", "first"]);
    fixture.ok(&["rm", "--wait", "first", "--yes"]);
    wait_until("the second restore", || paths[1].join("restored").exists());
    assert!(!paths[3].join("restored").exists());
    // Stopping everything also cancels the restore still waiting.
    fixture.ok(&["stop", "--all"]);
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));
    assert!(!paths[2].join("restored").exists());
}
