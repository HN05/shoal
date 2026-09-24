use crate::support::{Fixture, commit_resource_config, git, wait_until};
use serde_json::Value;
use std::{
    fs, io::Write, os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration,
};

#[test]
fn lifecycle_hooks_run_untracked_after_setup_and_before_removal() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let scripts = [
        ("setup.sh", "#!/bin/sh\nprintf setup > setup-done\n"),
        (
            "post-setup.sh",
            r#"#!/bin/sh
printf 'post-setup output\n'
test -z "$SHOAL_SCOPE_TOKEN" || exit 81
test -z "$SHOAL_EXECUTION_ID" || exit 82
test -z "$SHOAL_SHELL_DIRECTIVE" || exit 83
test "$SHOAL_HOOK" = post_setup || exit 84
test -f setup-done || exit 85
test -z "$SHOAL_TEST_FAIL_HOOK" || exit 5
printf '%s\n%s\n' "$PWD" "$SHOAL_WORKSPACE" > "$HOME/post-setup-ran"
sleep 30 < /dev/null > /dev/null 2>&1 &
"#,
        ),
        (
            "pre-remove.sh",
            r#"#!/bin/sh
test "$SHOAL_HOOK" = pre_remove || exit 86
test -z "$SHOAL_SCOPE_TOKEN" || exit 87
printf '%s\n%s\n' "$PWD" "$SHOAL_WORKSPACE" > "$HOME/pre-remove-ran"
test ! -f fail-removal || { echo 'session still busy' >&2; exit 3; }
"#,
        ),
    ];
    for (name, body) in scripts {
        fs::write(fixture.repo.join(name), body).unwrap();
        fs::set_permissions(fixture.repo.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    commit_resource_config(
        &fixture.repo,
        "setup_cmd = 'setup.sh'\npost_setup_cmd = 'post-setup.sh'\npre_remove_cmd = 'pre-remove.sh'\n",
    );
    let output = fixture
        .command()
        .args(["--json", "add", fixture.repo.to_str().unwrap(), "hooked"])
        .env("SHOAL_SHELL_DIRECTIVE", home.join("directive"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(workspace["state"], "ready");
    assert!(String::from_utf8_lossy(&output.stderr).contains("post-setup output"));
    let path = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
    assert_eq!(
        fs::read_to_string(home.join("post-setup-ran")).unwrap(),
        format!("{}\nhooked\n", path.display())
    );
    let inspection = fixture.ok(&["inspect", "hooked"]);
    assert_eq!(
        inspection["executions"],
        serde_json::json!([]),
        "hooks and what they leave behind are not tracked executions"
    );

    // A failing hook keeps the ready workspace and does not start the agent.
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("codex"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture
        .command()
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "hook-fails",
            "--agent",
            "codex",
        ])
        .env("SHOAL_TEST_FAIL_HOOK", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("post_setup_cmd exited with 5"));
    let failed = fixture.ok(&["inspect", "hook-fails"]);
    assert_eq!(failed["workspace"]["state"], "ready");
    let failed_path = Path::new(failed["workspace"]["path"].as_str().unwrap());
    assert!(!failed_path.join("agent-started").exists());
    assert_eq!(fixture.ok(&["setup", "hook-fails"])["state"], "ready");
    assert!(
        fs::read_to_string(home.join("post-setup-ran"))
            .unwrap()
            .ends_with("\nhook-fails\n")
    );

    // The removal hook runs in the daemon before the worktree goes; failure retains it.
    fs::write(path.join("fail-removal"), "busy").unwrap();
    let output = fixture.run(&["rm", "hooked", "--yes", "--delete-branch"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pre_remove_cmd exited with 3"), "{stderr}");
    assert!(stderr.contains("session still busy"), "{stderr}");
    assert_eq!(
        fs::read_to_string(home.join("pre-remove-ran")).unwrap(),
        format!("{}\nhooked\n", path.display())
    );
    let retained = fixture.ok(&["inspect", "hooked"]);
    assert_eq!(retained["workspace"]["state"], "ready");
    assert!(path.join("tracked").exists());
    fs::remove_file(path.join("fail-removal")).unwrap();
    fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
    assert!(!path.exists());
    assert!(
        !fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["name"] == "hooked")
    );
}

#[test]
fn resource_hooks_retain_leases_on_failure_and_run_during_removal() {
    let mut fixture = Fixture::new();
    fs::write(
        fixture.repo.join("permit.sh"),
        r#"#!/bin/sh
set -eu
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test "$PWD" = "$SHOAL_WORKSPACE_PATH"
printf '%s\n' "$SHOAL_RESOURCE_LEASE" >> "$HOME/$SHOAL_HOOK"
test ! -f "$SHOAL_HOOK-fails" || { echo 'resource busy' >&2; exit 3; }
"#,
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("permit.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(
        &fixture.repo,
        "post_resource_acquire_cmd = 'permit.sh'\npre_resource_release_cmd = 'permit.sh'\n[resources.signing]\n",
    );
    let workspace = fixture.add("hooked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fixture.add("waiter");
    fs::write(path.join("post_resource_acquire-fails"), "").unwrap();
    let output = fixture.run(&[
        "resource",
        "acquire",
        "signing",
        "hooked",
        "--reason",
        "test signing",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("resource lease retained"));
    let inspection = fixture.ok(&["inspect", "hooked"]);
    let lease = &inspection["resources"][0];
    assert_eq!(lease["reason"], "test signing");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    assert_eq!(
        fixture
            .run(&["resource", "acquire", "signing", "waiter"])
            .status
            .code(),
        Some(2)
    );
    fixture.restart();
    fs::remove_file(path.join("post_resource_acquire-fails")).unwrap();
    assert_eq!(
        &fixture.ok(&["resource", "acquire", "signing", "hooked"]),
        lease
    );
    let events = fs::read_to_string(fixture.root.path().join("post_resource_acquire")).unwrap();
    let events: Vec<Value> = events
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events, vec![lease.clone(), lease.clone()]);
    fs::write(path.join("pre_resource_release-fails"), "").unwrap();
    for args in [
        vec!["resource", "release", "signing", "hooked"],
        vec!["rm", "hooked", "--yes", "--delete-branch"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("resource busy"));
        assert_eq!(fixture.ok(&["inspect", "hooked"])["resources"][0], *lease);
        assert!(path.exists());
    }
    fs::remove_file(path.join("pre_resource_release-fails")).unwrap();
    // Removing the definition does not prevent release of an existing lease.
    fs::write(
        path.join(".shoal.toml"),
        "pre_resource_release_cmd = 'permit.sh'\n",
    )
    .unwrap();
    fixture.ok(&["resource", "release", "signing", "hooked"]);
    fixture.ok(&["resource", "acquire", "signing", "waiter"]);
    fixture.ok(&["rm", "waiter", "--yes", "--delete-branch"]);
    let events = fs::read_to_string(fixture.root.path().join("pre_resource_release")).unwrap();
    assert_eq!(events.lines().count(), 4);
    fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
}

#[test]
fn resource_hooks_exclude_concurrent_release_setup_and_removal() {
    let fixture = Fixture::new();
    fs::write(
        fixture.repo.join("permit.sh"),
        r#"#!/bin/sh
set -eu
touch "$HOME/hook-entered"
while test ! -f "$HOME/hook-continue"; do sleep 0.05; done
"#,
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("permit.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(
        &fixture.repo,
        "post_resource_acquire_cmd = 'permit.sh'\nsetup_cmd = '/usr/bin/true'\n[resources.signing]\n",
    );
    fixture.add("hooked");
    let acquire = fixture
        .command()
        .args(["resource", "acquire", "signing", "hooked"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("permit hook", || {
        fixture.root.path().join("hook-entered").exists()
    });
    for args in [
        vec!["resource", "acquire", "signing", "hooked"],
        vec!["resource", "release", "signing", "hooked"],
        vec!["setup", "hooked"],
        vec!["rm", "hooked", "--yes", "--delete-branch"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("resource operation is in progress"),
            "{output:?}"
        );
    }
    fs::write(fixture.root.path().join("hook-continue"), "").unwrap();
    assert!(acquire.wait_with_output().unwrap().status.success());
    fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
}

#[test]
fn pre_setup_hook_gates_readiness_and_supports_global_defaults() {
    let fixture = Fixture::with_config(Some("pre_setup_cmd = 'before.sh'\n"));
    fs::write(
        fixture.repo.join("before.sh"),
        r#"#!/bin/sh
set -eu
test "$SHOAL_HOOK" = pre_setup
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
echo before >> "$HOME/setup-order"
test ! -f "$HOME/fail-before" || { echo 'prepare failed' >&2; exit 7; }
"#,
    )
    .unwrap();
    fs::write(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\necho setup >> \"$HOME/setup-order\"\n",
    )
    .unwrap();
    for name in ["before.sh", "setup.sh"] {
        fs::set_permissions(fixture.repo.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    commit_resource_config(&fixture.repo, "setup_cmd = 'setup.sh'\n");
    let home = fixture.root.path();
    fs::write(home.join("fail-before"), "").unwrap();
    let output = fixture.run(&["add", fixture.repo.to_str().unwrap(), "hooked"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pre_setup_cmd exited with 7"));
    let inspection = fixture.ok(&["inspect", "hooked"]);
    assert_eq!(inspection["workspace"]["state"], "failed");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    assert_eq!(
        fs::read_to_string(home.join("setup-order")).unwrap(),
        "before\n"
    );
    fs::remove_file(home.join("fail-before")).unwrap();
    assert_eq!(fixture.ok(&["setup", "hooked"])["state"], "ready");
    assert_eq!(
        fs::read_to_string(home.join("setup-order")).unwrap(),
        "before\nbefore\nsetup\n"
    );
    let path = Path::new(inspection["workspace"]["path"].as_str().unwrap());
    // A repository override wins; a pre-setup hook alone still gates readiness.
    fs::write(
        path.join(".shoal.toml"),
        "pre_setup_cmd = '/usr/bin/true'\n",
    )
    .unwrap();
    assert_eq!(fixture.ok(&["setup", "hooked"])["state"], "ready");
    fs::write(fixture.repo.join(".shoal.toml"), "").unwrap();
    git(&fixture.repo, &["add", ".shoal.toml"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-m",
            "hook only",
        ],
    );
    assert_eq!(fixture.add("hook-only")["state"], "ready");
    assert_eq!(
        fs::read_to_string(home.join("setup-order")).unwrap(),
        "before\nbefore\nsetup\nbefore\n"
    );
    fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
    fixture.ok(&["rm", "hook-only", "--yes", "--delete-branch"]);
}

#[test]
fn post_remove_hook_uses_checkout_and_reports_failure_after_removal() {
    let fixture = Fixture::with_config(Some("post_remove_cmd = '/usr/bin/false'\n"));
    fs::write(
        fixture.repo.join("after removal.sh"),
        r#"#!/bin/sh
set -eu
test "$SHOAL_HOOK" = post_remove
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test ! -e "$SHOAL_WORKSPACE_PATH"
printf '%s\n%s\n%s\n' "$PWD" "$SHOAL_WORKSPACE" "$SHOAL_WORKSPACE_PATH" >> "$HOME/removed"
test ! -f "$HOME/fail-after" || { echo 'external cleanup failed' >&2; exit 8; }
"#,
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("after removal.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(
        &fixture.repo,
        "post_remove_cmd = 'after removal.sh'\n[resources.signing]\n",
    );
    let workspace = fixture.add("hooked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fixture.ok(&["resource", "acquire", "signing", "hooked"]);
    let config = path.join(".shoal.toml");
    fs::write(
        &config,
        "post_remove_cmd = 'after removal.sh'\npre_remove_cmd = '/usr/bin/false'\n",
    )
    .unwrap();
    assert!(
        !fixture
            .run(&["rm", "hooked", "--yes", "--delete-branch"])
            .status
            .success()
    );
    assert!(!fixture.root.path().join("removed").exists());
    fs::write(&config, "post_remove_cmd = 'after removal.sh'\n").unwrap();
    fs::write(fixture.root.path().join("fail-after"), "").unwrap();
    let result = fixture.ok(&["rm", "hooked", "--yes", "--delete-branch"]);
    assert_eq!(result["removed"], true);
    assert!(
        result["hook_error"]
            .as_str()
            .unwrap()
            .contains("external cleanup failed")
    );
    assert!(!fixture.run(&["inspect", "hooked"]).status.success());
    let notifications = fixture.ok(&["notifications"]);
    assert!(
        notifications
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "hook_failed" && n["workspace"] == "hooked")
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("removed")).unwrap(),
        format!("{}\nhooked\n{}\n", fixture.repo.display(), path.display())
    );
    fs::remove_file(fixture.root.path().join("fail-after")).unwrap();
    fixture.add("next");
    fixture.ok(&["resource", "acquire", "signing", "next"]);
    let missing = fixture.add("missing");
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    fixture.ok(&["rm", "missing", "--yes", "--keep-branch"]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("removed"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    // Repository removal invokes the hook before deleting its checkout.
    fixture.ok(&["repo", "rm", fixture.repo.to_str().unwrap(), "--yes"]);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("removed"))
            .unwrap()
            .lines()
            .count(),
        6
    );
}

#[test]
fn workspace_hook_resolution_preserves_layers_and_directories() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
    };
    let mut fixture = Fixture::with_config(Some(""));
    let workspace = fixture.add("hooks");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    // Global hooks are installed only after creation, so these paths need not
    // exist: resolution must not run a hook or require its executable yet.
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        "pre_setup_cmd = 'global'\npost_remove_cmd = 'global'\n\
         post_resource_acquire_cmd = 'global'\npre_resource_release_cmd = 'global'\n",
    )
    .unwrap();
    fixture.restart();
    let call = |request: Value| {
        let mut socket =
            UnixStream::connect(fixture.root.path().join("state/daemon.sock")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(socket, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let protocol =
        call(serde_json::json!({"protocol":0,"id":1,"method":"status"}))["protocol"].clone();
    let resolve = |kind: &str| {
        call(serde_json::json!({"protocol":protocol,"id":2,
            "method":{"workspace_hook":{"workspace":workspace["id"],"kind":kind}}}))
    };
    let saved = fixture.root.path().join("saved.toml");
    let save = |text: &str| {
        fs::write(&saved, text).unwrap();
        fixture.ok(&[
            "repo",
            "config",
            fixture.repo.to_str().unwrap(),
            "--file",
            saved.to_str().unwrap(),
        ]);
    };
    for (kind, global) in [
        ("setup", false),
        ("pre_setup", true),
        ("post_setup", false),
        ("pre_remove", false),
        ("post_remove", true),
        ("post_resource_acquire", true),
        ("pre_resource_release", true),
    ] {
        let directory = if kind == "post_remove" {
            &fixture.repo
        } else {
            worktree
        };
        let file = worktree.join(".shoal.toml");
        fs::write(&file, "").unwrap();
        save("");
        let result = resolve(kind);
        assert_eq!(result["type"], "hook", "{result}");
        assert_eq!(
            result["data"],
            if global {
                serde_json::json!(directory.join("global"))
            } else {
                Value::Null
            },
            "{kind}"
        );

        fs::write(&file, format!("{kind}_cmd = 'file hook'\n")).unwrap();
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(directory.join("file hook"))
        );
        // An unrelated saved option must not mask the worktree's hook.
        save("default_agent = 'claude'\n");
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(directory.join("file hook"))
        );
        save(&format!("{kind}_cmd = 'saved hook'\n"));
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(directory.join("saved hook"))
        );
        save(&format!("{kind}_cmd = '/absolute/hook'\n"));
        assert_eq!(resolve(kind)["data"], "/absolute/hook");
    }
}
