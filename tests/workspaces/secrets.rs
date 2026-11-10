//! Secrets and the API proxy.
use super::approvals::pending_approval;
use super::*;

/// A workspace `proxied` whose Claude goes through a proxy with its key in
/// `CLIPROXY_TEST_KEY`, and a `claude` that records its arguments.
fn proxied_claude() -> Fixture {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let config = home.join(".config/shoal/config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(
        "[machine.proxies.cliproxy]\nupstream = 'http://127.0.0.1:9'\n\
         key = { env = 'CLIPROXY_TEST_KEY' }\n\
         \n[machine.agents.claude]\nproxy = 'cliproxy'",
    );
    fs::write(&config, text).unwrap();
    let reload = fixture.run(&["daemon", "reload"]);
    assert!(reload.status.success(), "{reload:?}");
    fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "proxied",
    ]);
    fixture.add("proxied");
    install_test_script(
        home.join("bin/claude"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/claude-args\"\nenv > \"$HOME/claude-env\"\n\
         previous=\nfor arg in \"$@\"; do\n\
         if [ \"$previous\" = --settings ]; then cat \"$arg\" > \"$HOME/claude-settings\"; fi\n\
         previous=$arg\ndone\n",
    );
    fixture
}

#[test]
fn a_proxied_agent_takes_its_key_from_its_environment_or_does_not_start() {
    let fixture = proxied_claude();
    let home = fixture.root.path();

    // Without its key the agent never starts, and its execution ends.
    let output = fixture
        .command()
        .args(["claude", "proxied"])
        .env_remove("CLIPROXY_TEST_KEY")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(!home.join("claude-args").exists());
    let status = fixture.ok(&["work", "status", "proxied"]);
    assert_eq!(status["executions"], serde_json::json!([]), "{status}");

    // With it, Claude Code is pointed at the proxy on a loopback port.
    let output = fixture
        .command()
        .args(["claude", "proxied"])
        .env("CLIPROXY_TEST_KEY", "sk-test")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let args = fs::read_to_string(home.join("claude-args")).unwrap();
    let settings = fs::read_to_string(home.join("claude-settings")).unwrap();
    assert!(
        settings.contains("\"ANTHROPIC_BASE_URL\":\"http://127.0.0.1:"),
        "{settings}"
    );
    // The settings, which may hold the user's tokens, stay off the command
    // line.
    assert!(!args.contains("ANTHROPIC_BASE_URL"), "{args}");
    assert!(!args.contains("sk-test"), "{args}");
    // Its requests name the proxy's key from its private environment, not
    // its command line, which any local user can read.
    let env = fs::read_to_string(home.join("claude-env")).unwrap();
    let header = env
        .lines()
        .find_map(|line| line.strip_prefix("ANTHROPIC_CUSTOM_HEADERS="))
        .unwrap_or_else(|| panic!("{env}"));
    let key = header.strip_prefix("x-shoal-proxy-key: ").unwrap();
    assert_eq!(key.len(), 32, "{header}");
    assert!(!args.contains(key), "{args}");
}

#[test]
fn secrets_reach_agents_but_not_ordinary_executions_or_hooks() {
    let fixture = Fixture::with_config(Some(
        "[secrets.AGENT_TOKEN]\ncommand = ['printf', '%s\\n', 'global secret']\n",
    ));
    let workspace = fixture.add("secrets");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    install_test_script(
        worktree.join("post-setup.sh"),
        "#!/bin/sh\nprintf '%s|%s' \"${AGENT_TOKEN-unset}\" \"${SHOAL_SECRET_ENV-unset}\" > hook-env\n",
    );
    install_test_script(
        worktree.join("setup.sh"),
        "#!/bin/sh\nprintf '%s' \"${AGENT_TOKEN-unset}\" > setup-env\n",
    );
    fs::write(
        worktree.join(".shoal.toml"),
        "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\n[agents.claude]\ncommand = ['sh', '-c', 'printf \"%s|%s\\n\" \"$AGENT_TOKEN\" \"$SHOAL_SECRET_ENV\"']\n",
    )
    .unwrap();
    let output = fixture.run(&["claude", "secrets"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"global secret|AGENT_TOKEN\n");
    let output = fixture.run(&[
        "run",
        "--workspace",
        "secrets",
        "--",
        "printenv",
        "AGENT_TOKEN",
    ]);
    assert!(!output.status.success(), "{output:?}");

    // Setup and its hook started beneath an agent never get the secrets it
    // inherited: the daemon starts them, and none travel through it.
    let output = fixture
        .command()
        .env("AGENT_TOKEN", "inherited")
        .env("SHOAL_SECRET_ENV", "AGENT_TOKEN")
        .args(["work", "setup", "secrets"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(worktree.join("hook-env")).unwrap(),
        "unset|unset"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("setup-env")).unwrap(),
        "unset"
    );

    // The local config can replace a secret; a failing command
    // stops the launch.
    let saved = fixture.root.path().join("saved.toml");
    fs::write(&saved, "[secrets.AGENT_TOKEN]\ncommand = ['false']\n").unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    let output = fixture.run(&["claude", "secrets"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    // Secret sources describe this machine, so a tracked file naming one is
    // rejected and its secret is never used.
    fs::write(
        worktree.join(".shoal.toml"),
        "[secrets.OTHER]\ncommand = ['true']\n",
    )
    .unwrap();
    let output = fixture.run(&["config", "show", "secrets"]);
    assert!(output.status.success());
    assert!(!String::from_utf8(output.stdout).unwrap().contains("OTHER"));
    let warning = String::from_utf8(output.stderr).unwrap();
    assert!(warning.contains("[secrets] belongs"), "{warning}");
}

#[test]
fn tool_secrets_reach_only_their_tools() {
    let fixture = Fixture::with_config(Some(
        "[secrets.TOOL_TOKEN]\ncommand = ['printf', 'tool secret']\ntools = ['show-token']\n",
    ));
    let workspace = fixture.add("tool-secrets");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("show-token"),
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"${TOOL_TOKEN-unset}\" \"$SHOAL_SECRET_ENV\" \"$*\"\n",
    );
    install_test_script(worktree.join("setup.sh"), "#!/bin/sh\n");
    install_test_script(
        worktree.join("post-setup.sh"),
        "#!/bin/sh\nshow-token hook > hook-output\nprintf '%s' \"${SHOAL_SECRET_TOOLS-unset}\" >> hook-output\n",
    );
    fs::write(
        worktree.join(".shoal.toml"),
        "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\n[agents.claude]\ncommand = ['sh', '-c', 'printf \"%s\\n\" \"${TOOL_TOKEN-unset}\"; show-token \"two words\" -x']\n",
    )
    .unwrap();
    let output = fixture.run(&["claude", "tool-secrets"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "unset\ntool secret|TOOL_TOKEN|two words -x\n"
    );
    let output = fixture.run(&["run", "--workspace", "tool-secrets", "--", "show-token"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"unset||\n");
    assert!(
        fs::read_dir(fixture.root.path().join("state"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("secret-tools-"))
    );

    // A hook started beneath an agent runs the tool without its launcher.
    let launchers = fixture.root.path().join("launchers");
    install_test_script(launchers.join("show-token"), "#!/bin/sh\necho launcher\n");
    let path = std::env::join_paths([launchers.clone(), bin].into_iter().chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let output = fixture
        .command()
        .env("PATH", path)
        .env("SHOAL_SECRET_TOOLS", &launchers)
        .args(["work", "setup", "tool-secrets"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(worktree.join("hook-output")).unwrap(),
        "unset||hook\nunset"
    );

    // An inner launch's launcher hands over to the enclosing launch's, which
    // runs the tool itself rather than returning to the inner one.
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("show-tokens"),
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$INNER_TOKEN\" \"$OUTER_TOKEN\" \"$SHOAL_SECRET_ENV\"\n",
    );
    let mut path = Vec::new();
    for name in ["INNER", "OUTER"] {
        let launchers = fixture.root.path().join(name.to_lowercase());
        fs::create_dir_all(&launchers).unwrap();
        fs::write(
            launchers.join(".secrets.json"),
            format!(
                r#"{{"{name}_TOKEN": {{"command": ["printf", "{name}"], "tools": ["show-tokens"]}}}}"#
            ),
        )
        .unwrap();
        install_test_script(
            launchers.join("show-tokens"),
            format!(
                "#!/bin/sh\nexec {:?} internal secret-tool {:?} show-tokens -- \"$@\"\n",
                support::shoal_binary(),
                launchers
            ),
        );
        path.push(launchers);
    }
    path.push(bin);
    let path = std::env::join_paths(path.into_iter().chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let output = fixture
        .command()
        .env("PATH", path)
        .arg("internal")
        .arg("secret-tool")
        .arg(fixture.root.path().join("inner"))
        .args(["show-tokens", "--"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"INNER|OUTER|INNER_TOKEN:OUTER_TOKEN\n");
}

#[test]
fn gated_tool_secrets_wait_for_the_users_approval() {
    let fixture = Fixture::with_config(Some(
        "[secrets.TOOL_TOKEN]\ncommand = ['sh', '-c', 'echo fetched >> \"$0\"; printf secret', 'fetches']\ntools = ['show-token']\nrequires_approval = true\n",
    ));
    let workspace = fixture.add("gated");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    install_test_script(
        fixture.root.path().join("bin").join("show-token"),
        "#!/bin/sh\nprintf '%s\\n' \"$TOOL_TOKEN\"\n",
    );
    fs::write(
        worktree.join(".shoal.toml"),
        "[agents.claude]\ncommand = ['show-token']\n",
    )
    .unwrap();
    // The catalog shows what an agent may request, never how it is fetched.
    let catalog = scoped_command(&fixture, "gated", &["catalog", "secret"]);
    assert!(catalog.status.success(), "{catalog:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&catalog.stdout).unwrap(),
        serde_json::json!({"secrets": [{
            "name": "TOOL_TOKEN", "tools": ["show-token"],
            "requires_approval": true, "approval_lifetime": "lease"
        }]})
    );
    let rendered = fixture.run(&["catalog", "secret", "--workspace", "gated"]);
    assert_eq!(
        String::from_utf8_lossy(&rendered.stdout),
        "TOOL_TOKEN: tools: show-token; approval required (lease)\n"
    );
    let launch = || {
        let output = fixture.run(&["claude", "gated"]);
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.stdout,
        )
    };
    let (success, _, _) = launch();
    assert!(!success);

    let request = pending_approval(scoped_command(
        &fixture,
        "gated",
        &["acquire", "secret", "TOOL_TOKEN", "--reason", "sign in"],
    ));
    assert_eq!(request["target"], "secret/TOOL_TOKEN");
    let id = request["number"].as_str().unwrap().to_owned();
    let id = id.as_str();
    let (success, _, _) = launch();
    assert!(!success);
    // The value is fetched only once the daemon confirms the grant.
    assert!(!worktree.join("fetches").exists());

    fixture.ok(&["approvals", "--approve", id]);
    let (success, stderr, stdout) = launch();
    assert!(success, "{stderr}");
    assert_eq!(stdout, b"secret\n");
    assert!(worktree.join("fetches").exists());

    fixture.ok(&["release", "--workspace", "gated", "secret", "TOOL_TOKEN"]);
    let (success, _, _) = launch();
    assert!(!success);

    // A launcher from before approval was required still follows current policy.
    let launchers = fixture.root.path().join("ungated");
    fs::create_dir_all(&launchers).unwrap();
    fs::write(
        launchers.join(".secrets.json"),
        r#"{"TOOL_TOKEN": {"command": ["sh", "-c", "echo fetched >> \"$0\"; printf secret", "fetches"], "tools": ["show-token"]}}"#,
    )
    .unwrap();
    let output = fixture.run(&[
        "run",
        "--workspace",
        "gated",
        "--",
        support::shoal_binary(),
        "internal",
        "secret-tool",
        launchers.to_str().unwrap(),
        "show-token",
        "--",
    ]);
    assert!(!output.status.success());
}
