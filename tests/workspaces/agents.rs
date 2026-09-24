use crate::support::Fixture;
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[test]
fn add_starts_agents_only_after_creation_and_preserves_workspace_on_exit() {
    let fixture = Fixture::new();
    let bin = fixture.root.path().join("add-agent-bin");
    fs::create_dir(&bin).unwrap();
    let inspection = fixture.root.path().join("agent-inspection.json");
    let directive = fixture.root.path().join("directive");
    for agent in ["codex", "claude"] {
        let stub = bin.join(agent);
        fs::write(
            &stub,
            r#"#!/bin/sh
"$SHOAL_TEST_BIN" --state-dir "$SHOAL_TEST_STATE" --json inspect "$SHOAL_TEST_NAME" > "$SHOAL_TEST_INSPECTION" || exit 99
test -f tracked || exit 98
test -z "$SHOAL_SHELL_DIRECTIVE" || exit 97
printf '%s\n' "$PWD" "$SHOAL_WORKSPACE" "$@"
exit 7
"#,
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    for (name, agent, mode) in [
        ("add-codex", "codex", "cli"),
        ("add-claude", "claude", "cli"),
        ("add-app", "codex", "app"),
    ] {
        fs::write(
            config_dir.join("config.toml"),
            format!("[codex]\ndefault_mode = '{mode}'"),
        )
        .unwrap();
        let output = fixture
            .command()
            .args([
                "add",
                fixture.repo.to_str().unwrap(),
                name,
                "--agent",
                agent,
                "--",
                "literal spaces; $(false)",
            ])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SHOAL_TEST_BIN", env!("CARGO_BIN_EXE_shoal"))
            .env("SHOAL_TEST_STATE", fixture.root.path().join("state"))
            .env("SHOAL_TEST_NAME", name)
            .env("SHOAL_TEST_INSPECTION", &inspection)
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        let during: Value = serde_json::from_slice(&fs::read(&inspection).unwrap()).unwrap();
        assert_eq!(during["workspace"]["name"], name);
        assert_eq!(
            during["executions"].as_array().unwrap().len(),
            usize::from(mode == "cli")
        );
        let after = fixture.ok(&["inspect", name]);
        assert_eq!(after["executions"], serde_json::json!([]));
        let path = after["workspace"]["path"].as_str().unwrap();
        assert!(Path::new(path).join("tracked").exists());
        assert_eq!(fs::read_to_string(&directive).unwrap(), format!("{path}\n"));
        let args = if mode == "app" {
            format!("\napp\n{path}\nliteral spaces; $(false)\n")
        } else if agent == "claude" {
            format!("{name}\nliteral spaces; $(false)\n--remote-control\n{name}\n")
        } else {
            format!(
                "{name}\nliteral spaces; $(false)\n--sandbox\ndanger-full-access\n--ask-for-approval=never\n"
            )
        };
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .ends_with(&format!(
                    "{}\n{args}",
                    fs::canonicalize(path).unwrap().display()
                ))
        );
    }

    fs::remove_file(&inspection).unwrap();
    let output = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "bad-base",
            "--base",
            "missing-ref",
            "--agent",
            "codex",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("SHOAL_TEST_BIN", env!("CARGO_BIN_EXE_shoal"))
        .env("SHOAL_TEST_INSPECTION", &inspection)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!inspection.exists());

    // A missing executable leaves a completed worktree available for retry.
    fs::remove_file(bin.join("codex")).unwrap();
    fs::write(config_dir.join("config.toml"), "").unwrap();
    let output = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "missing-agent",
            "--agent",
            "codex",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let workspace = fixture.ok(&["inspect", "missing-agent"]);
    assert!(
        Path::new(workspace["workspace"]["path"].as_str().unwrap())
            .join("tracked")
            .exists()
    );
    assert_eq!(workspace["executions"], serde_json::json!([]));
    assert!(
        !fixture
            .run(&["add", "--", "prompt without agent"])
            .status
            .success()
    );
}

#[test]
fn codex_default_mode_is_read_at_launch_and_explicit_modes_override_it() {
    let fixture = Fixture::new();
    let workspace = fixture.add("default-mode");
    let path = workspace["path"].as_str().unwrap();
    let bin = fixture.root.path().join("codex-bin");
    fs::create_dir(&bin).unwrap();
    let stub = bin.join("codex");
    fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$SHOAL_WORKSPACE\" \"$@\"\nexit 7\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();

    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    for config in ["", "[codex]\ndefault_mode = 'app'"] {
        // Change the default while the daemon remains running.
        fs::write(config_dir.join("config.toml"), config).unwrap();
        for mode in [None, Some("--cli"), Some("--app")] {
            let mut command = fixture.command();
            command.current_dir(path).arg("codex");
            if let Some(mode) = mode {
                command.args([mode, "default-mode"]);
            }
            let output = command
                .args(["--", "literal spaces; $(false)"])
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(7),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let app = mode == Some("--app") || (mode.is_none() && !config.is_empty());
            let expected = if app {
                format!("\napp\n{path}\nliteral spaces; $(false)\n")
            } else {
                "default-mode\nliteral spaces; $(false)\n--sandbox\ndanger-full-access\n--ask-for-approval=never\n".into()
            };
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            assert_eq!(
                fixture.ok(&["inspect", "default-mode"])["executions"],
                serde_json::json!([])
            );
        }
    }
    // The worktree's own config wins over the global default, also at launch.
    fs::write(config_dir.join("config.toml"), "").unwrap();
    fs::write(
        Path::new(path).join(".shoal.toml"),
        "[codex]\ndefault_mode = 'app'\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .current_dir(path)
        .args(["codex", "--", "repo-default"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("\napp\n{path}\nrepo-default\n")
    );
}

#[test]
fn agent_shortcuts_forward_arguments_without_starting_real_agents() {
    let fixture = Fixture::new();
    let workspace = fixture.add("shortcut");
    let bin = fixture.root.path().join("stub-bin");
    fs::create_dir(&bin).unwrap();
    for agent in ["claude", "codex"] {
        let path = bin.join(agent);
        fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$SHOAL_WORKSPACE\" \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = fixture.command();
        command.arg(agent);
        if agent == "codex" {
            command.arg("--cli");
        }
        let output = command
            .args(["shortcut", "--", "--version", "hello with spaces"])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "shortcut\n--version\nhello with spaces\n{}",
                if agent == "claude" {
                    "--remote-control\nshortcut\n"
                } else {
                    "--sandbox\ndanger-full-access\n--ask-for-approval=never\n"
                }
            )
        );
    }
    // Current-directory resolution supplies an ID internally; Claude still gets
    // the human workspace name, as it must after an fzf selection as well.
    for target in [Some(workspace["id"].as_str().unwrap()), None] {
        let mut command = fixture.command();
        command
            .current_dir(workspace["path"].as_str().unwrap())
            .arg("claude");
        if let Some(target) = target {
            command.arg(target);
        }
        let output = command
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"shortcut\n--remote-control\nshortcut\n");
    }
}

#[test]
fn agent_templates_resolve_per_launch_and_reach_native_instruction_options() {
    let fixture = Fixture::new();
    let workspace = fixture.add("instructions");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("agent-template.md"),
        "global {workspace} {branch}",
    )
    .unwrap();
    let bin = fixture.root.path().join("template-bin");
    fs::create_dir(&bin).unwrap();
    for agent in ["codex", "claude"] {
        let stub = bin.join(agent);
        fs::write(&stub, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n").unwrap();
        fs::set_permissions(stub, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (index, expected) in [
        "global instructions instructions".to_owned(),
        format!("repo {}", path.display()),
        "saved \"quotes\"\n$(false) {unknown}".to_owned(),
        String::new(),
    ]
    .into_iter()
    .enumerate()
    {
        if index == 1 {
            fs::write(path.join("agent-template.md"), "repo {path}").unwrap();
        }
        if index >= 2 {
            let local = fixture.root.path().join("local.toml");
            fs::write(
                &local,
                format!("agent_template = {}", toml::Value::String(expected.clone())),
            )
            .unwrap();
            fixture.ok(&[
                "repo",
                "config",
                fixture.repo.to_str().unwrap(),
                "--file",
                local.to_str().unwrap(),
            ]);
        }
        for agent in ["codex", "claude"] {
            let mut command = fixture.command();
            command.arg(agent);
            if agent == "codex" {
                command.arg("--cli");
            }
            let output = command
                .args(["instructions", "--", "user prompt"])
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let args: Vec<_> = stdout.split('\0').collect();
            if expected.is_empty() {
                assert_eq!(args[0], "user prompt");
            } else {
                if agent == "claude" {
                    assert_eq!(args[..2], ["--append-system-prompt", &expected]);
                } else {
                    assert_eq!(args[0], "-c");
                    let setting: toml::Value = toml::from_str(args[1]).unwrap();
                    assert_eq!(
                        setting["developer_instructions"].as_str(),
                        Some(expected.as_str())
                    );
                }
                assert_eq!(args[2], "user prompt");
            }
        }
    }
}

#[test]
fn desktop_shortcuts_open_workspaces_without_cli_flags_or_execution_records() {
    let fixture = Fixture::new();
    let workspace = fixture.add("desktop");
    let path = workspace["path"].as_str().unwrap();
    let bin = fixture.root.path().join("desktop-bin");
    fs::create_dir(&bin).unwrap();
    for program in ["codex", "t3"] {
        let stub = bin.join(program);
        fs::write(&stub, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\nexit 7\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
        for explicit in [true, false] {
            let mut command = fixture.command();
            command.arg(program);
            if program == "codex" {
                command.arg("--app");
            }
            if explicit {
                command.arg("desktop");
            } else {
                command.current_dir(path);
            }
            let output = command
                .args(["--", "--example", "literal spaces; $(false)"])
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
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
                fixture.ok(&["inspect", "desktop"])["executions"],
                serde_json::json!([])
            );
        }
    }
    fixture.add("other");
    for args in [vec!["codex", "other", "--app"], vec!["t3", "other"]] {
        let output = fixture
            .command()
            .args(["exec", "desktop", "--", env!("CARGO_BIN_EXE_shoal")])
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("another worktree"));
    }
    assert!(!fixture.run(&["codex", "desktop"]).status.success());
}

#[test]
fn claude_launch_marks_the_workspace_trusted_in_claude_config() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let workspace = fixture.add("trusted");
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let key = path.to_str().unwrap();
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("claude"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let config = home.join(".claude.json");

    // First launch creates a trusted entry even before Claude has a config.
    assert!(fixture.run(&["claude", "trusted"]).status.success());
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(root["projects"][key]["hasTrustDialogAccepted"], true);

    fs::write(&config, r#"{"numStartups": 1, "projects": {}}"#).unwrap();
    assert!(fixture.run(&["claude", "trusted"]).status.success());
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(root["numStartups"], 1);
    assert_eq!(root["projects"][key]["hasTrustDialogAccepted"], true);

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
    assert_eq!(overridden["projects"][key]["hasTrustDialogAccepted"], true);
    let untouched: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert!(untouched["projects"][key].is_null());

    // A bad override warns and still launches.
    let output = fixture
        .command()
        .args(["claude", "trusted"])
        .env("CLAUDE_CONFIG_DIR", "relative")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be an absolute path"));
}

#[test]
fn codex_launches_trust_the_workspace_in_the_selected_user_config() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let workspace = fixture.add("trusted");
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    let key = path.to_str().unwrap();
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // Inspect the config from the child so trust must precede the launch.
    fs::write(
        bin.join("codex"),
        "#!/bin/sh\ncat \"${CODEX_HOME:-$HOME/.codex}/config.toml\"\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let config = home.join(".codex/config.toml");
    for mode in [None, Some("--cli"), Some("--app")] {
        for overridden in [false, true] {
            let config_dir = home.join("custom-codex");
            let target = if overridden {
                config_dir.join("config.toml")
            } else {
                config.clone()
            };
            let mut command = fixture.command();
            command.arg("codex");
            if let Some(mode) = mode {
                command.args([mode, "trusted"]);
            } else {
                command.current_dir(&path);
            }
            if overridden {
                command.env("CODEX_HOME", &config_dir);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let root: toml::Value =
                toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
            assert_eq!(
                root["projects"][key]["trust_level"].as_str(),
                Some("trusted")
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
    let output = fixture.run(&["codex", "trusted", "--cli"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not mark"));
    assert_eq!(fs::read_to_string(config).unwrap(), "projects = []\n");
}

#[test]
fn cli_agent_command_defaults_can_be_replaced_at_launch() {
    let fixture = Fixture::new();
    let workspace = fixture.ok(&["add", fixture.repo.to_str().unwrap(), "configured-agent"]);
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join(".shoal.toml"),
        "[commands]\nclaude = ['printf', '%s\\n', '{workspace}', '{args}', '{branch}', '{path}']\ncodex = ['printf', '%s\\n', '{args}', 'custom default']\n"
    ).unwrap();
    let output = fixture.run(&["claude", "configured-agent", "--", "{path}", "two words"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "configured-agent\n{{path}}\ntwo words\nconfigured-agent\n{}\n",
            path.display()
        )
    );
    let output = fixture.run(&[
        "run",
        "claude",
        "configured-agent",
        "--",
        "{path}",
        "two words",
    ]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "configured-agent\n{{path}}\ntwo words\nconfigured-agent\n{}\n",
            path.display()
        )
    );
    let output = fixture.run(&[
        "codex",
        "configured-agent",
        "--cli",
        "--",
        "--model",
        "example",
    ]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"--model\nexample\ncustom default\n");
    let output = fixture.run(&[
        "run",
        "codex",
        "configured-agent",
        "--",
        "--model",
        "example",
    ]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"--model\nexample\ncustom default\n");
}

#[test]
fn agent_auth_wrappers_are_inherited_without_changing_ordinary_executions() {
    let fixture = Fixture::new();
    let workspace = fixture.add("agent-auth");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let wrappers = fixture.root.path().join("wrappers with spaces");
    fs::create_dir(&wrappers).unwrap();
    for tool in ["fj", "gh"] {
        let path = wrappers.join(tool);
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' 'agent {tool}' \"$@\"\n"),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = format!(
        "[commands]\nclaude = ['sh', '-c', 'fj \"two words\" \"$literal\"; gh auth status; printf \"%s\\n\" \"$HOME\"; \"$SHOAL_TEST_BINARY\" exec -- fj nested']\n[agent_auth]\nfj = {:?}\ngh = {:?}\n",
        wrappers.join("fj"),
        wrappers.join("gh")
    );
    fs::write(worktree.join(".shoal.toml"), config).unwrap();
    let report = fixture.ok(&["config", "show", "agent-auth"]);
    let fj = report
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "agent_auth.fj")
        .unwrap();
    assert_eq!(fj["value"], wrappers.join("fj").to_str().unwrap());
    assert_eq!(fj["layer"], "worktree_file");
    let output = fixture
        .command()
        .env("SHOAL_TEST_BINARY", env!("CARGO_BIN_EXE_shoal"))
        .env("literal", "$() ; ' literal")
        .args(["claude", "agent-auth"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "agent fj\ntwo words\n$() ; ' literal\nagent gh\nauth\nstatus\n{}\nagent fj\nnested\n",
            fixture.root.path().display()
        )
    );
    let expected = fixture
        .command()
        .get_envs()
        .find(|(key, _)| *key == "PATH")
        .unwrap()
        .1
        .unwrap()
        .to_owned();
    let output = fixture.run(&["exec", "agent-auth", "--", "printenv", "PATH"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        expected.to_str().unwrap()
    );
    assert!(
        fs::read_dir(fixture.root.path().join("state"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("agent-auth-"))
    );
}

#[test]
fn detached_agents_use_auth_wrappers_and_invalid_wrappers_prevent_launch() {
    let fixture = Fixture::new();
    let workspace = fixture.add("detached-auth");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let wrapper = fixture.root.path().join("fj-agent");
    fs::write(
        &wrapper,
        "#!/bin/sh\nprintf '%s\\n' 'detached agent' \"$@\"\nexit 23\n",
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        worktree.join(".shoal.toml"),
        "[agent_auth]\nfj = '~/fj-agent'\n",
    )
    .unwrap();
    let log = fixture.root.path().join("agent.log");
    let output = fixture.run(&[
        "detached-internal",
        "detached-auth",
        "--log",
        log.to_str().unwrap(),
        "--agent",
        "happy claude",
        "--",
        "fj",
        "two words",
    ]);
    assert_eq!(
        output.status.code(),
        Some(23),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(&log)
            .unwrap()
            .contains("detached agent\ntwo words\n")
    );
    fs::remove_file(&wrapper).unwrap();
    let output = fixture.run(&["claude", "detached-auth"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("agent_auth.fj"));
    let status = fixture.ok(&["inspect", "detached-auth"]);
    assert!(
        status["executions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["state"] != "running")
    );
}

#[test]
fn custom_agents_launch_with_layered_prompts_scope_and_notifications() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'pi'\nagent_template = 'Follow {branch}'\nissue_template = '{title}: {body}'\n[commands]\npi = ['missing-global-launcher']\n",
    ));
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, script) in [
        (
            "gh",
            "#!/bin/sh\nprintf '%s' '{\"number\":37,\"title\":\"Literal {branch}\",\"body\":\"$(false)\"}'\n",
        ),
        (
            "fake-agent",
            "#!/bin/sh\ntest -n \"$SHOAL_SCOPE_TOKEN\" || exit 99\nprintf '%s\\0' \"$@\" > \"$HOME/agent-args\"\nexit 7\n",
        ),
    ] {
        let path = bin.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fixture.add_github_origin();
    let saved = fixture.root.path().join("saved.toml");
    fs::write(
        &saved,
        "[commands]\npi = ['fake-agent', '--prompt={prompt}', '{args}', '{branch}']\n",
    )
    .unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    let output = fixture.run(&[
        "--json",
        "issue",
        "37",
        "--repo",
        fixture.repo.to_str().unwrap(),
        "--base",
        "HEAD",
        "--",
        "literal {prompt}",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let workspace: Value =
        serde_json::from_slice(output.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    let branch = workspace["branch"].as_str().unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("agent-args")).unwrap(),
        format!(
            "--prompt=Follow {branch}\n\nLiteral {{branch}}: $(false)\0literal {{prompt}}\0{branch}\0"
        )
    );
    assert_eq!(
        fixture.ok(&["inspect", workspace["id"].as_str().unwrap()])["executions"],
        serde_json::json!([])
    );
    let notifications = fixture.ok(&["notifications"]);
    assert!(notifications.to_string().contains("pi exited with code 7"));
    assert!(!fixture.root.path().join(".claude.json").exists());
    assert!(!fixture.root.path().join(".codex/config.toml").exists());

    let output = fixture.run(&[
        "run",
        "pi",
        workspace["id"].as_str().unwrap(),
        "--",
        "literal {prompt}",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("agent-args")).unwrap(),
        format!("--prompt=\0literal {{prompt}}\0{branch}\0")
    );

    // Without an explicit prompt slot, context precedes literal forwarded arguments.
    fs::write(&saved, "[commands]\npi = ['fake-agent', '{args}']\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    let output = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "custom-add",
        "--base",
        "HEAD",
        "--agent",
        "pi",
        "--",
        "user message",
    ]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("agent-args")).unwrap(),
        "Follow custom-add\0user message\0"
    );
    let before = fixture.ok(&["list"]);
    let unknown = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "unknown-agent",
        "--agent",
        "typo",
    ]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown agent"));
    assert_eq!(fixture.ok(&["list"]), before);
}
