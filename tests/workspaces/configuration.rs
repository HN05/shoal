//! Global, repository and local configuration, its layers and reloads, and
//! Git profiles.
#[cfg(target_os = "macos")]
use super::fixture::{FORGEJO_HOST, NO_GRACE, NO_OVERLOAD_WARNINGS, UNSETTLED};
use super::*;

#[test]
fn config_show_reports_effective_values_and_their_layers() {
    let fixture = Fixture::with_config(Some(
        "agent.default = 'claude'\n[commands]\nglobal = ['global']\nshared = ['global']\n\
         [cleanup.auto]\nenabled = false\n[ports]\nstart = 2000\nend = 6000\n",
    ));

    // In a registered checkout without a workspace, the checkout file is the
    // worktree layer for the current-directory target.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "hooks.post_setup = 'checkout/attach'\n",
    )
    .unwrap();
    let checkout = fixture
        .command()
        .current_dir(&fixture.repo)
        .args(["--json", "config", "show"])
        .output()
        .unwrap();
    assert!(checkout.status.success());
    let checkout: Value = serde_json::from_slice(&checkout.stdout).unwrap();
    let checkout_entry = checkout
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "hooks.post_setup")
        .unwrap();
    assert_eq!(checkout_entry["value"], "checkout/attach");
    assert_eq!(checkout_entry["layer"], "worktree_file");
    fs::remove_file(fixture.repo.join(".shoal.toml")).unwrap();

    let workspace = fixture.add("layered-config");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "setup.command = 'scripts/setup'\n[commands]\nworktree = ['worktree']\nshared = ['worktree']\n\
         [ports]\nstart = 3000\n[ports.web]\nenv = 'WORKTREE_PORT'\n",
    )
    .unwrap();
    let saved = fixture.root.path().join("saved-config.toml");
    fs::write(
        &saved,
        "agent.default = 'codex'\n[commands]\nshared = ['saved']\n[ports]\nend = 4000\n",
    )
    .unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);

    let report = fixture.ok(&["config", "show", "layered-config"]);
    let entry = |key: &str| {
        report
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == key)
            .unwrap()
    };
    for (key, value, layer) in [
        ("agent.default", serde_json::json!("codex"), "local_config"),
        (
            "commands.shared",
            serde_json::json!(["saved"]),
            "local_config",
        ),
        ("ports.start", serde_json::json!(3000), "worktree_file"),
        ("ports.end", serde_json::json!(4000), "local_config"),
        (
            "commands.global",
            serde_json::json!(["global"]),
            "global_config",
        ),
        (
            "cleanup.auto.idle_minutes",
            serde_json::json!(10),
            "built_in_default",
        ),
    ] {
        assert_eq!(entry(key)["value"], value, "{key}");
        assert_eq!(entry(key)["layer"], layer, "{key}");
    }

    // A named repository is shown as in its registered checkout.
    let repository = fixture.ok(&["config", "show", "--repo", fixture.repo.to_str().unwrap()]);
    let entry = |key: &str| {
        repository
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == key)
            .unwrap()
    };
    assert_eq!(entry("agent.default")["value"], "codex");
    assert_eq!(entry("agent.default")["layer"], "local_config");
}

#[test]
fn the_daemon_alerts_about_global_config_problems_on_start_and_reload() {
    let environment = |fixture: &Fixture| -> Vec<String> {
        fixture
            .ok(&["alerts", "--all", "--keep-unread"])
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "environment_problem")
            .map(|event| event["message"].as_str().unwrap().to_owned())
            .collect()
    };
    let reported = |fixture: &Fixture, setting: &str| {
        environment(fixture)
            .iter()
            .any(|message| message.contains(setting))
    };
    let fixture = Fixture::with_config(Some(
        "rules.worker.text = 'You are working in Shoal workspace {workspace} on branch {branch} at {path}.'\n",
    ));
    // The startup check reads the config before it records its alerts, so
    // this one shows it is past reading by the time the config changes.
    wait_until("the startup environment alert", || {
        reported(&fixture, "rules.worker.text")
    });
    // A problem added later is reported only by the reload. A root inside a
    // file cannot be created.
    let root_file = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        format!(
            "machine.root_dir = {:?}\n",
            root_file.path().join("repos").to_str().unwrap()
        ),
    )
    .unwrap();
    assert!(!reported(&fixture, "root_dir"));
    fixture.ok(&["daemon", "reload"]);
    wait_until("the reload environment alert", || {
        reported(&fixture, "root_dir")
    });
}

#[test]
#[cfg(target_os = "macos")]
fn global_config_changes_reload_a_running_daemon() {
    let fixture = Fixture::with_tools(None, true);
    fixture.add("worker");
    let policy = || fixture.ok(&["catalog", "sim", "--workspace", "worker"])["policy"].clone();
    assert!(policy()["default"].is_null());
    let result = fixture.ok(&[
        "config",
        "set",
        "machine.simulators.profiles.phone.device",
        "iPhone 17",
        "machine.simulators.profiles.phone.runtime",
        "iOS 27",
        "machine.simulators.default",
        "phone",
    ]);
    assert_eq!(result["daemon_reloaded"], true);
    assert_eq!(policy()["default"], "phone");
    let catalog = fixture.ok(&["catalog", "--workspace", "worker"]);
    assert_eq!(catalog["simulators"]["policy"], policy());
    // Hand edits apply on an explicit reload; an invalid file keeps the running config.
    let config = fixture.root.path().join(".config/shoal/config.toml");
    let edited = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!("{edited}\n[machine.simulators]\nmax_booted = 0\n"),
    )
    .unwrap();
    assert!(!fixture.run(&["daemon", "reload"]).status.success());
    assert_eq!(policy()["default"], "phone");
    fs::write(
        &config,
        format!("{NO_GRACE}{UNSETTLED}{FORGEJO_HOST}{NO_OVERLOAD_WARNINGS}"),
    )
    .unwrap();
    assert!(fixture.run(&["daemon", "reload"]).status.success());
    assert!(policy()["default"].is_null());
}

#[test]
fn inline_repository_config_resolves_relative_paths_in_the_callers_directory() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .current_dir(&fixture.repo)
        .args([
            "--json",
            "config",
            "set",
            "agent.default",
            "claude",
            "--repo",
            ".",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["config"],
        fixture.ok(&["repo", "config", fixture.repo.to_str().unwrap()])["path"]
    );
}

#[test]
fn inline_repository_config_edits_preserve_layers_and_serialize_updates() {
    let mut fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "agent.default = 'codex'\n");
    fixture.add("worker");
    let repo = fixture.ok(&["repo", "ls"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let edited = fixture.ok(&["config", "set", "agent.default", "claude", "--repo", id]);
    let saved = fixture.ok(&["repo", "config", id]);
    assert_eq!(edited["config"], saved["path"]);
    let agent = || {
        fixture
            .ok(&["config", "show", "worker"])
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == "agent.default")
            .unwrap()
            .clone()
    };
    assert_eq!(agent()["value"], "claude");
    assert_eq!(agent()["layer"], "local_config");
    for (key, value) in [
        ("cleanup.auto.enabled", "maybe"),
        ("root_dir", "/tmp/no"),
        ("default_agnet", "codex"),
        ("agents.claude.prompt_cache_minutes", "0"),
        ("agents.list.command", "['echo']"),
        ("commands.claude", "['echo']"),
        ("agents.loop.base", "loop"),
        ("agents.helper.base", "list"),
        ("agents.helper.base", "all"),
        ("agents.claude.resume", "['--resume']"),
    ] {
        assert!(
            !fixture
                .run(&["config", "set", key, value, "--repo", id])
                .status
                .success(),
            "{key} = {value}"
        );
        assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    }
    // One invalid change rejects the whole set.
    assert!(
        !fixture
            .run(&[
                "config",
                "set",
                "cleanup.auto.idle_minutes",
                "30",
                "cleanup.auto.enabled",
                "maybe",
                "--repo",
                id,
            ])
            .status
            .success()
    );
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    fixture.ok(&[
        "config",
        "set",
        "cleanup.auto.idle_minutes",
        "30",
        "agent.default",
        "claude",
        "--repo",
        id,
    ]);
    fixture.ok(&["config", "unset", "cleanup", "agent.default", "--repo", id]);
    assert_eq!(agent()["value"], "codex");
    assert_eq!(agent()["layer"], "worktree_file");
    assert_eq!(
        fs::read_to_string(fixture.repo.join(".shoal.toml")).unwrap(),
        "agent.default = 'codex'\n"
    );
    thread::scope(|scope| {
        for index in 0..8 {
            let fixture = &fixture;
            scope.spawn(move || {
                fixture.ok(&[
                    "config",
                    "set",
                    &format!("commands.check{index}"),
                    "['true']",
                    "--repo",
                    id,
                ]);
            });
        }
    });
    let saved = fixture.ok(&["repo", "config", id]);
    let config: toml::Value = toml::from_str(saved["toml"].as_str().unwrap()).unwrap();
    assert_eq!(config["commands"].as_table().unwrap().len(), 8);
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
}

#[test]
fn local_config_is_a_file_shared_by_every_workspace_and_read_in_place() {
    let mut fixture = Fixture::new();
    let first = fixture.add("first");
    let repo = fixture.ok(&["repo", "ls"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let shown = fixture.ok(&["repo", "config", id]);
    assert!(shown["toml"].is_null());
    let local = PathBuf::from(shown["path"].as_str().unwrap());
    assert!(local.ends_with(".shoal/config.toml"));
    assert_eq!(
        local.parent().unwrap().parent().unwrap(),
        Path::new(first["path"].as_str().unwrap()).parent().unwrap()
    );
    let input = fixture.root.path().join("local.toml");
    let text = "# Local preferences\n[ports.web]\nenv='LOCAL_PORT'\n[resources.lock]\ncapacity=1\n";
    fs::write(&input, text).unwrap();
    let saved = fixture.save_local_config(id, &input);
    assert_eq!(saved["toml"], text);
    let printed = fixture.run(&["repo", "config", id]).stdout;
    assert_eq!(
        String::from_utf8(printed).unwrap(),
        format!("{}\n{text}", local.display())
    );
    fixture.ok(&["repo", "rename", id, "renamed"]);
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "config", "renamed"]), saved);
    fixture.add("second");
    for path in [&fixture.repo, Path::new(first["path"].as_str().unwrap())] {
        assert!(git(path, &["status", "--porcelain"]).is_empty());
        assert!(!path.join(".shoal.toml").exists());
        assert!(!path.join(".shoal").exists());
    }
    for name in ["first", "second"] {
        assert_eq!(
            fixture.ok(&["catalog", "port", "--workspace", name])["configured"]["web"]["env"],
            "LOCAL_PORT"
        );
    }

    // Edits keep comments and the previous file, and apply at once.
    let edited = fixture.ok(&["config", "set", "ports.web.env", "EDITED", "--repo", id]);
    assert_eq!(edited["config"], local.to_str().unwrap());
    assert_eq!(
        fs::read_to_string(local.with_extension("toml.backup")).unwrap(),
        text
    );
    assert!(
        fs::read_to_string(&local)
            .unwrap()
            .starts_with("# Local preferences\n")
    );
    assert_eq!(
        fixture.ok(&["catalog", "port", "--workspace", "first"])["configured"]["web"]["env"],
        "EDITED"
    );
    // An edit that leaves an invalid file changes nothing.
    for (key, value) in [
        ("ports.web.port", "0"),
        ("resources.lock.capacity", "0"),
        ("unknown", "true"),
    ] {
        let before = fs::read_to_string(&local).unwrap();
        assert!(
            !fixture
                .run(&["config", "set", key, value, "--repo", id])
                .status
                .success()
        );
        assert_eq!(fs::read_to_string(&local).unwrap(), before);
    }

    let path = Path::new(first["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[ports.checked_in]\nenv='CHECKED_IN'\n",
    )
    .unwrap();
    let configured = fixture.ok(&["catalog", "port", "--workspace", "first"])["configured"].clone();
    assert_eq!(configured.as_object().unwrap().len(), 2);
    assert_eq!(configured["web"]["env"], "EDITED");
    assert_eq!(configured["checked_in"]["env"], "CHECKED_IN");
    // A file edited by hand into an invalid one fails every lookup until fixed.
    fs::write(&local, "invalid TOML").unwrap();
    assert!(
        !fixture
            .run(&["catalog", "port", "--workspace", "first"])
            .status
            .success()
    );
    // An empty file sets nothing, so every option falls through.
    fs::write(&local, "").unwrap();
    let configured = fixture.ok(&["catalog", "port", "--workspace", "first"])["configured"].clone();
    assert_eq!(configured.as_object().unwrap().len(), 1);
    assert_eq!(configured["checked_in"]["env"], "CHECKED_IN");
    assert!(fixture.clear_local_config(id)["toml"].is_null());
}

#[test]
fn local_config_scripts_live_beside_it_and_go_with_the_repository() {
    let fixture = Fixture::new();
    let repo = fixture.ok(&["repo", "ls"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let local = PathBuf::from(
        fixture.ok(&["repo", "config", id])["path"]
            .as_str()
            .unwrap(),
    );
    let directory = local.parent().unwrap();
    fs::create_dir_all(directory).unwrap();
    // A relative hook path names a script beside the local config.
    install_test_script(
        directory.join("pre-setup.sh"),
        "#!/bin/sh\ntouch \"$PWD/pre-setup-ran\"\n",
    );
    fs::write(&local, "hooks.pre_setup = 'pre-setup.sh'\n").unwrap();
    let workspace = fixture.add("scripted");
    assert!(
        Path::new(workspace["path"].as_str().unwrap())
            .join("pre-setup-ran")
            .exists()
    );
    let other_path = fixture.root.path().join("other-repo");
    fs::create_dir(&other_path).unwrap();
    git(&other_path, &["init", "-b", "main"]);
    let other = fixture.ok(&["repo", "add", other_path.to_str().unwrap()]);
    assert!(fixture.ok(&["repo", "config", other["id"].as_str().unwrap()])["toml"].is_null());
    fixture.ok(&["rm", "scripted", "--yes"]);
    fixture.ok(&["repo", "rm", id, "--yes"]);
    assert!(!directory.exists());
    assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 1);
}

#[test]
fn git_profiles_layer_and_isolate_worktree_settings_before_setup() {
    let fixture = Fixture::with_config(Some(
        "git_profile = 'personal'\n[machine.git_profiles.personal]\nuser.email = 'personal@example.invalid'\n\
         [machine.git_profiles.work]\nuser.name = 'Work Name'\nuser.email = 'work@example.invalid'\ncommit.gpgsign = false\n",
    ));
    git(
        &fixture.repo,
        &["config", "user.email", "main@example.invalid"],
    );
    let personal = fixture.add("personal");
    let personal_path = Path::new(personal["path"].as_str().unwrap());
    assert_eq!(
        git(personal_path, &["config", "user.email"]).trim(),
        "personal@example.invalid"
    );
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\ngit config user.email > setup-email\n",
    );
    commit_resource_config(
        &fixture.repo,
        "git_profile = 'work'\nsetup.command = 'setup.sh'\n",
    );
    let work = fixture.add("work");
    let work_path = Path::new(work["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(work_path.join("setup-email"))
            .unwrap()
            .trim(),
        "work@example.invalid"
    );
    assert_eq!(
        git(work_path, &["config", "--worktree", "commit.gpgsign"]).trim(),
        "false"
    );
    assert_eq!(
        git(&fixture.repo, &["config", "user.email"]).trim(),
        "main@example.invalid"
    );
    assert_eq!(
        git(personal_path, &["config", "user.email"]).trim(),
        "personal@example.invalid"
    );
    let saved = fixture.root.path().join("local.toml");
    fs::write(&saved, "git_profile = 'personal'\n").unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    git(&fixture.repo, &["branch", "existing"]);
    let existing = fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "existing",
    ]);
    let existing_path = Path::new(existing["path"].as_str().unwrap());
    assert_eq!(
        git(existing_path, &["config", "user.email"]).trim(),
        "personal@example.invalid"
    );
    // Reopening neither reapplies a changed profile nor runs setup again.
    git(
        existing_path,
        &[
            "config",
            "--worktree",
            "user.email",
            "edited@example.invalid",
        ],
    );
    fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "existing",
    ]);
    assert_eq!(
        git(existing_path, &["config", "user.email"]).trim(),
        "edited@example.invalid"
    );
}

#[test]
fn git_profiles_leave_unselected_repositories_alone_and_retain_failed_workspaces() {
    let fixture = Fixture::new();
    let plain = fixture.add("plain");
    assert!(
        !Path::new(plain["git_dir"].as_str().unwrap())
            .join("config.worktree")
            .exists()
    );
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "config",
                "--default",
                "false",
                "--get",
                "extensions.worktreeConfig"
            ]
        )
        .trim(),
        "false"
    );
    commit_resource_config(&fixture.repo, "git_profile = 'missing'\n");
    let output = fixture.run(&["start", fixture.repo.to_str().unwrap(), "unknown"]);
    assert!(!output.status.success());
    let failed = fixture.ok(&["status", "unknown"]);
    assert_eq!(failed["workspace"]["state"], "failed");
    assert!(Path::new(failed["workspace"]["path"].as_str().unwrap()).is_dir());
    fixture.ok(&["rm", "unknown", "--yes"]);
}

#[test]
fn git_profile_flag_overrides_config_for_new_and_existing_branches() {
    let fixture = Fixture::with_config(Some(
        "[machine.git_profiles.manual]\nuser.email = 'manual@example.invalid'\n",
    ));
    commit_resource_config(&fixture.repo, "git_profile = 'missing'\n");
    git(&fixture.repo, &["branch", "existing-profile"]);
    for (existing, name) in [(false, "new-profile"), (true, "existing-profile")] {
        let mut args = vec!["start", fixture.repo.to_str().unwrap()];
        if existing {
            args.push("--existing");
        }
        args.extend([name, "--git-profile", "manual"]);
        let workspace = fixture.ok(&args);
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(
            git(path, &["config", "user.email"]).trim(),
            "manual@example.invalid"
        );
        let output = fixture.run(&[
            "start",
            fixture.repo.to_str().unwrap(),
            "--existing",
            name,
            "--git-profile",
            "manual",
        ]);
        assert!(!output.status.success());
        assert_eq!(
            git(path, &["config", "user.email"]).trim(),
            "manual@example.invalid"
        );
    }
    let before = fixture.ok(&["list"]);
    let output = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "bad-profile",
        "--git-profile",
        "unknown",
    ]);
    assert!(!output.status.success());
    assert_eq!(fixture.ok(&["list"]), before);
    assert!(git(&fixture.repo, &["branch", "--list", "bad-profile"]).is_empty());
}

#[test]
fn agent_git_profile_applies_only_to_agent_processes() {
    let fixture = Fixture::with_config(Some(
        "[machine.git_profiles.agent]\nuser.name = 'Agent Name'\nuser.email = 'agent@example.invalid'\n",
    ));
    git(
        &fixture.repo,
        &["config", "user.email", "main@example.invalid"],
    );
    let workspace = fixture.add("agent-git");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let commands = "[agents.claude]\ncommand = ['sh', '-c', 'git config user.email; git config core.abbrev; git var GIT_AUTHOR_IDENT; git var GIT_COMMITTER_IDENT']\n";
    fs::write(
        worktree.join(".shoal.toml"),
        format!("{commands}[agent]\ngit_profile = 'agent'\n"),
    )
    .unwrap();
    let output = fixture
        .command()
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.abbrev")
        .env("GIT_CONFIG_VALUE_0", "12")
        .env("GIT_AUTHOR_EMAIL", "inherited@example.invalid")
        .env("GIT_COMMITTER_NAME", "Inherited Name")
        .args(["claude", "agent-git"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{stdout}");
    assert_eq!(lines[..2], ["agent@example.invalid", "12"], "{stdout}");
    for ident in &lines[2..] {
        assert!(
            ident.starts_with("Agent Name <agent@example.invalid> "),
            "{stdout}"
        );
    }
    let output = fixture.run(&[
        "run",
        "--workspace",
        "agent-git",
        "--",
        "git",
        "config",
        "user.email",
    ]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"main@example.invalid\n");
    fs::write(
        worktree.join(".shoal.toml"),
        format!("{commands}[agent]\ngit_profile = 'missing'\n"),
    )
    .unwrap();
    let output = fixture.run(&["claude", "agent-git"]);
    assert!(!output.status.success());
}

/// An agent's Git over HTTPS to a host with an agent token gets the token
/// from Shoal's helper, which the user's own helper neither replaces nor
/// saves; other processes keep the user's login.
#[test]
fn agents_git_over_https_uses_the_hosts_agent_token() {
    let fixture = Fixture::with_config(Some(
        "[machine.forges.\"git.example.com\"]\ntype = \"forgejo\"\nagent_token = \"agenttoken\"\n",
    ));
    let saved = fixture.root.path().join("git-credentials");
    fs::write(&saved, "https://me:usertoken@git.example.com\n").unwrap();
    let store = format!("store --file={}", saved.display());
    git(&fixture.repo, &["config", "credential.helper", &store]);
    let workspace = fixture.add("agent-login");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    let fill = "printf 'protocol=https\\nhost=git.example.com\\n\\n' | GIT_TERMINAL_PROMPT=0 git credential fill";
    let approve = "printf 'protocol=https\\nhost=git.example.com\\nusername=token\\npassword=agenttoken\\n\\n' | git credential approve";
    fs::write(
        worktree.join(".shoal.toml"),
        format!("[agents.claude]\ncommand = ['sh', '-c', \"{fill}; {approve}\"]\n"),
    )
    .unwrap();

    let agent = fixture.run(&["claude", "agent-login"]);
    let other = fixture.run(&["run", "--workspace", "agent-login", "--", "sh", "-c", fill]);

    assert!(agent.status.success(), "{agent:?}");
    let stdout = String::from_utf8(agent.stdout).unwrap();
    assert!(stdout.contains("password=agenttoken\n"), "{stdout}");
    assert_eq!(
        fs::read_to_string(&saved).unwrap(),
        "https://me:usertoken@git.example.com\n"
    );
    assert!(other.status.success(), "{other:?}");
    let stdout = String::from_utf8(other.stdout).unwrap();
    assert!(stdout.contains("password=usertoken\n"), "{stdout}");
}
