#[cfg(target_os = "macos")]
use super::simulators::SIM_CONFIG;
use crate::support::{Fixture, commit_resource_config, git};
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, thread};

#[test]
fn config_show_reports_effective_values_and_their_layers() {
    let fixture = Fixture::with_config(Some(
        "default_agent = 'claude'\n[commands]\nglobal = ['global']\nshared = ['global']\n\
         [auto_cleanup]\nenabled = false\n[ports]\nstart = 2000\nend = 6000\n",
    ));

    // In a registered checkout without a workspace, the checkout file is the
    // worktree layer for the current-directory target.
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "post_setup_cmd = 'checkout/attach'\n",
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
        .find(|entry| entry["key"] == "post_setup_cmd")
        .unwrap();
    assert_eq!(checkout_entry["value"], "checkout/attach");
    assert_eq!(checkout_entry["layer"], "worktree_file");
    fs::remove_file(fixture.repo.join(".shoal.toml")).unwrap();

    let workspace = fixture.add("layered-config");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "setup_cmd = 'scripts/setup'\n[commands]\nworktree = ['worktree']\nshared = ['worktree']\n\
         [ports]\nstart = 3000\n[ports.web]\nenv = 'WORKTREE_PORT'\n",
    )
    .unwrap();
    let saved = fixture.root.path().join("saved-config.toml");
    fs::write(
        &saved,
        "default_agent = 'codex'\n[commands]\nshared = ['saved']\n[ports]\nend = 4000\n",
    )
    .unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);

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
        (
            "default_agent",
            serde_json::json!("codex"),
            "saved_repository_config",
        ),
        (
            "commands.shared",
            serde_json::json!(["saved"]),
            "saved_repository_config",
        ),
        (
            "commands.worktree",
            serde_json::json!(["worktree"]),
            "worktree_file",
        ),
        (
            "setup_cmd",
            serde_json::json!("scripts/setup"),
            "worktree_file",
        ),
        ("ports.start", serde_json::json!(3000), "worktree_file"),
        (
            "ports.end",
            serde_json::json!(4000),
            "saved_repository_config",
        ),
        (
            "commands.global",
            serde_json::json!(["global"]),
            "global_config",
        ),
        (
            "auto_cleanup.enabled",
            serde_json::json!(false),
            "global_config",
        ),
        (
            "codex.default_mode",
            serde_json::json!("cli"),
            "built_in_default",
        ),
        (
            "auto_cleanup.idle_minutes",
            serde_json::json!(10),
            "built_in_default",
        ),
    ] {
        assert_eq!(entry(key)["value"], value, "{key}");
        assert_eq!(entry(key)["layer"], layer, "{key}");
    }
    assert_eq!(entry("ports.web")["value"]["env"], "WORKTREE_PORT");
    assert_eq!(entry("ports.web")["layer"], "worktree_file");

    let human = fixture.run(&["config", "show", "layered-config"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("ports.start = 3000 (worktree file)"));
    assert!(human.contains("default_agent = \"codex\" (saved repository config)"));
}

#[test]
fn inline_repository_config_resolves_relative_paths_in_the_callers_directory() {
    let fixture = Fixture::new();
    for args in [
        vec!["config", "set", "default_agent", "claude", "--repo", "."],
        vec!["config", "unset", "default_agent", "--repo", "."],
    ] {
        let output = fixture
            .command()
            .current_dir(&fixture.repo)
            .arg("--json")
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result["repository_id"],
            fixture.ok(&["repo", "list"])[0]["id"]
        );
        assert_eq!(
            result,
            fixture.ok(&["repo", "config", fixture.repo.to_str().unwrap()])
        );
    }
}

#[test]
fn inline_repository_config_edits_preserve_layers_and_serialize_updates() {
    let mut fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "default_agent = 'codex'\n");
    fixture.add("worker");
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let saved = fixture.ok(&["config", "set", "default_agent", "claude", "--repo", id]);
    assert_eq!(saved["repository_id"], id);
    let agent = || {
        fixture
            .ok(&["config", "show", "worker"])
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == "default_agent")
            .unwrap()
            .clone()
    };
    assert_eq!(agent()["value"], "claude");
    assert_eq!(agent()["layer"], "saved_repository_config");
    for (key, value) in [
        ("auto_cleanup.enabled", "maybe"),
        ("root_dir", "/tmp/no"),
        ("default_agnet", "codex"),
    ] {
        assert!(
            !fixture
                .run(&["config", "set", key, value, "--repo", id])
                .status
                .success()
        );
        assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    }
    fixture.ok(&["config", "unset", "default_agent", "--repo", id]);
    assert_eq!(agent()["value"], "codex");
    assert_eq!(agent()["layer"], "worktree_file");
    assert_eq!(
        fs::read_to_string(fixture.repo.join(".shoal.toml")).unwrap(),
        "default_agent = 'codex'\n"
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
fn local_repository_config_is_copied_shared_persistent_and_reversible() {
    let mut fixture = Fixture::new();
    let first = fixture.add("first");
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    assert!(fixture.ok(&["repo", "config", id])["toml"].is_null());
    let input = fixture.root.path().join("local.toml");
    let text = "# Local preferences\n[ports.web]\nenv='LOCAL_PORT'\n[resources.lock]\ncapacity=1\n";
    fs::write(&input, text).unwrap();
    let saved = fixture.ok(&["repo", "config", id, "--file", input.to_str().unwrap()]);
    assert_eq!(saved["repository_id"], id);
    assert_eq!(saved["toml"], text);
    assert_eq!(fixture.run(&["repo", "config", id]).stdout, text.as_bytes());
    fs::remove_file(&input).unwrap();
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
            fixture.ok(&["port", name])["configured"]["web"]["env"],
            "LOCAL_PORT"
        );
    }
    let port = fixture.ok(&["port", "acquire", "web", "first"]);
    assert_eq!(port["env_var"], "LOCAL_PORT");
    fixture.ok(&["resource", "acquire", "lock", "first"]);
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "lock", "second"])
            .status
            .code(),
        Some(2)
    );
    fixture.ok(&["resource", "release", "lock", "first"]);
    fixture.ok(&["rm", "second", "--yes", "--delete-branch"]);
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);

    let path = Path::new(first["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[ports.checked_in]\nenv='CHECKED_IN'\n",
    )
    .unwrap();
    fs::create_dir(path.join(".shoal")).unwrap();
    fs::write(path.join(".shoal/config.toml"), "invalid TOML").unwrap();
    // The worktree file is a layer of its own; its errors show through the saved config.
    assert!(!fixture.run(&["port", "first"]).status.success());
    fs::remove_file(path.join(".shoal/config.toml")).unwrap();
    let configured = fixture.ok(&["port", "first"])["configured"].clone();
    assert_eq!(configured.as_object().unwrap().len(), 2);
    assert_eq!(configured["web"]["env"], "LOCAL_PORT");
    assert_eq!(configured["checked_in"]["env"], "CHECKED_IN");
    // Layered names must agree: the saved `lock` resource meets a `lock` pool.
    fs::write(
        path.join(".shoal.toml"),
        "[resource_pools.lock.resources.a]\ncapacity=1\n",
    )
    .unwrap();
    assert!(!fixture.run(&["port", "first"]).status.success());
    fs::write(
        path.join(".shoal.toml"),
        "[ports.checked_in]\nenv='CHECKED_IN'\n",
    )
    .unwrap();
    // Invalid replacements must leave the saved config intact.
    for invalid in [
        "invalid TOML",
        "unknown=true",
        "[ports.web]\nport=0",
        "[resources.lock]\ncapacity=0",
    ] {
        fs::write(&input, invalid).unwrap();
        assert!(
            !fixture
                .run(&["repo", "config", id, "--file", input.to_str().unwrap()])
                .status
                .success()
        );
        assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    }
    // An empty saved config sets nothing, so every option falls through.
    fs::write(&input, "").unwrap();
    fixture.ok(&["repo", "config", id, "--file", input.to_str().unwrap()]);
    let configured = fixture.ok(&["port", "first"])["configured"].clone();
    assert_eq!(configured.as_object().unwrap().len(), 1);
    assert_eq!(configured["checked_in"]["env"], "CHECKED_IN");
    assert!(fixture.ok(&["repo", "config", id, "--clear"])["toml"].is_null());
    assert_eq!(
        fixture.ok(&["port", "first"])["configured"]["checked_in"]["env"],
        "CHECKED_IN"
    );
    assert_eq!(fixture.ok(&["port", "first"])["reserved"][0], port);
    fixture.ok(&["repo", "config", id, "--clear"]);
}

#[test]
fn local_repository_config_is_isolated_and_deleted_only_with_its_repository() {
    let fixture = Fixture::new();
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let input = fixture.root.path().join("local.toml");
    fs::write(&input, "[ports.web]\n").unwrap();
    let saved = fixture.ok(&["repo", "config", id, "--file", input.to_str().unwrap()]);
    let other_path = fixture.root.path().join("other-repo");
    fs::create_dir(&other_path).unwrap();
    git(&other_path, &["init", "-b", "main"]);
    let other = fixture.ok(&["repo", "add", other_path.to_str().unwrap()]);
    assert!(fixture.ok(&["repo", "config", other["id"].as_str().unwrap()])["toml"].is_null());
    let outside = fixture.root.path().join("outside");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "outside",
            outside.to_str().unwrap(),
        ],
    );
    assert!(!fixture.run(&["repo", "rm", id, "--yes"]).status.success());
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    git(
        &fixture.repo,
        &["worktree", "remove", outside.to_str().unwrap()],
    );
    fixture.ok(&["repo", "rm", id, "--yes"]);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM repository_configs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(input.exists());
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
}

#[test]
#[cfg(target_os = "macos")]
fn local_repository_config_selects_simulator_preferences() {
    let fixture = Fixture::with_tools(Some(SIM_CONFIG), true);
    fixture.add("worker");
    let input = fixture.root.path().join("local.toml");
    fs::write(&input, "[simulators]\npreferred=['tablet']\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        input.to_str().unwrap(),
    ]);
    let sim = fixture.ok(&["sim", "acquire", "worker"]);
    assert_eq!(sim["device"], "type.Tablet");
}

#[test]
fn git_profiles_layer_and_isolate_worktree_settings_before_setup() {
    let fixture = Fixture::with_config(Some(
        "git_profile = 'personal'\n[git.profiles.personal]\nuser.email = 'personal@example.invalid'\n\
         [git.profiles.work]\nuser.name = 'Work Name'\nuser.email = 'work@example.invalid'\ncommit.gpgsign = false\n",
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
    fs::write(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\ngit config user.email > setup-email\n",
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("setup.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(
        &fixture.repo,
        "git_profile = 'work'\nsetup_cmd = 'setup.sh'\n",
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
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    git(&fixture.repo, &["branch", "existing"]);
    let existing = fixture.ok(&[
        "add",
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
        "add",
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
    let output = fixture.run(&["add", fixture.repo.to_str().unwrap(), "unknown"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("git profile missing is not defined"));
    let failed = fixture.ok(&["inspect", "unknown"]);
    assert_eq!(failed["workspace"]["state"], "failed");
    assert!(Path::new(failed["workspace"]["path"].as_str().unwrap()).is_dir());
    fixture.ok(&["rm", "unknown", "--yes"]);
}

#[test]
fn git_profile_flag_overrides_config_for_new_and_existing_branches() {
    let fixture = Fixture::with_config(Some(
        "[git.profiles.manual]\nuser.email = 'manual@example.invalid'\n",
    ));
    commit_resource_config(&fixture.repo, "git_profile = 'missing'\n");
    git(&fixture.repo, &["branch", "existing-profile"]);
    for (existing, name) in [(false, "new-profile"), (true, "existing-profile")] {
        let mut args = vec!["add", fixture.repo.to_str().unwrap()];
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
            "add",
            fixture.repo.to_str().unwrap(),
            "--existing",
            name,
            "--git-profile",
            "manual",
        ]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("applies only to new worktrees"));
        assert_eq!(
            git(path, &["config", "user.email"]).trim(),
            "manual@example.invalid"
        );
    }
    let before = fixture.ok(&["list"]);
    let output = fixture.run(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "bad-profile",
        "--git-profile",
        "unknown",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("git profile unknown is not defined"));
    assert_eq!(fixture.ok(&["list"]), before);
    assert!(git(&fixture.repo, &["branch", "--list", "bad-profile"]).is_empty());
}
