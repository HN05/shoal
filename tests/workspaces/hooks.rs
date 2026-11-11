//! Lifecycle and resource hooks, and how their layers resolve.
use super::*;

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
# macOS hides an orphaned sleep's environment from concurrent setup checks.
python3 -c 'import time; time.sleep(30)' < /dev/null > /dev/null 2>&1 &
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
        install_test_script(fixture.repo.join(name), body);
    }
    commit_resource_config(
        &fixture.repo,
        "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\nhooks.pre_remove = 'pre-remove.sh'\n",
    );
    let output = fixture
        .command()
        .args(["--json", "start", fixture.repo.to_str().unwrap(), "hooked"])
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
    let inspection = fixture.ok(&["status", "hooked"]);
    assert_eq!(
        inspection["executions"],
        serde_json::json!([]),
        "hooks and what they leave behind are not tracked executions"
    );

    // A failing hook keeps the ready workspace and does not start the agent.
    let bin = home.join("bin");
    install_test_script(
        bin.join("codex"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    );
    let output = fixture
        .command()
        .args([
            "start",
            fixture.repo.to_str().unwrap(),
            "hook-fails",
            "--here",
            "--agent",
            "codex",
        ])
        .env("SHOAL_TEST_FAIL_HOOK", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let failed = fixture.ok(&["status", "hook-fails"]);
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
    let output = fixture.run(&["rm", "--wait", "hooked", "--yes"]);
    assert!(!output.status.success());
    // The hook's own error reaches the user.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("session still busy"), "{stderr}");
    assert_eq!(
        fs::read_to_string(home.join("pre-remove-ran")).unwrap(),
        format!("{}\nhooked\n", path.display())
    );
    let retained = fixture.ok(&["status", "hooked"]);
    assert_eq!(retained["workspace"]["state"], "ready");
    assert!(path.join("tracked").exists());
    fs::remove_file(path.join("fail-removal")).unwrap();
    fixture.ok(&["rm", "--wait", "hooked", "--yes"]);
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
    install_test_script(
        fixture.repo.join("permit.sh"),
        r#"#!/bin/sh
set -eu
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test "$PWD" = "$SHOAL_WORKSPACE_PATH"
printf '%s\n' "$SHOAL_RESOURCE_LEASE" >> "$HOME/$SHOAL_HOOK"
test ! -f "$SHOAL_HOOK-fails" || { echo 'resource busy' >&2; exit 3; }
"#,
    );
    commit_resource_config(
        &fixture.repo,
        "hooks.post_resource_acquire = 'permit.sh'\nhooks.pre_resource_release = 'permit.sh'\n[resources.signing]\n",
    );
    let workspace = fixture.add("hooked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fixture.add("waiter");
    fs::write(path.join("post_resource_acquire-fails"), "").unwrap();
    let output = fixture.run(&[
        "acquire",
        "resource",
        "signing",
        "--workspace",
        "hooked",
        "--reason",
        "test signing",
    ]);
    assert!(!output.status.success());
    let inspection = fixture.ok(&["status", "hooked"]);
    let lease = &inspection["resources"][0];
    assert_eq!(lease["reason"], "test signing");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    assert_eq!(
        fixture
            .run(&["acquire", "resource", "signing", "--workspace", "waiter"])
            .status
            .code(),
        Some(2)
    );
    fixture.restart();
    fs::remove_file(path.join("post_resource_acquire-fails")).unwrap();
    assert_eq!(
        &fixture.ok(&["acquire", "resource", "signing", "--workspace", "hooked"]),
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
        vec![
            "release",
            "resource",
            "signing",
            "--workspace",
            "hooked",
            "--name",
            "default",
        ],
        vec!["rm", "--wait", "hooked", "--yes"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("resource busy"));
        assert_eq!(fixture.ok(&["status", "hooked"])["resources"][0], *lease);
        assert!(path.exists());
    }
    fs::remove_file(path.join("pre_resource_release-fails")).unwrap();
    // Removing the definition does not prevent release of an existing lease.
    fs::write(
        path.join(".shoal.toml"),
        "hooks.pre_resource_release = 'permit.sh'\n",
    )
    .unwrap();
    fixture.ok(&[
        "release",
        "resource",
        "signing",
        "--workspace",
        "hooked",
        "--name",
        "default",
    ]);
    fixture.ok(&["acquire", "resource", "signing", "--workspace", "waiter"]);
    fixture.ok(&["rm", "--wait", "waiter", "--yes"]);
    let events = fs::read_to_string(fixture.root.path().join("pre_resource_release")).unwrap();
    assert_eq!(events.lines().count(), 4);
    fixture.ok(&["rm", "--wait", "hooked", "--yes"]);
}

#[test]
fn resource_hooks_exclude_concurrent_release_setup_and_removal() {
    let fixture = Fixture::new();
    install_test_script(
        fixture.repo.join("permit.sh"),
        r#"#!/bin/sh
set -eu
touch "$HOME/hook-entered"
while test ! -f "$HOME/hook-continue"; do sleep 0.05; done
"#,
    );
    commit_resource_config(
        &fixture.repo,
        "hooks.post_resource_acquire = 'permit.sh'\nsetup.command = '/usr/bin/true'\n[resources.signing]\n",
    );
    fixture.add("hooked");
    let acquire = fixture
        .command()
        .args(["acquire", "resource", "signing", "--workspace", "hooked"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("permit hook", || {
        fixture.root.path().join("hook-entered").exists()
    });
    let leases = fixture.ok(&["status", "hooked"])["resources"].clone();
    assert_eq!(leases.as_array().unwrap().len(), 1);
    assert_eq!(leases[0]["resource"], "signing");
    for args in [
        vec!["acquire", "resource", "signing", "--workspace", "hooked"],
        vec![
            "release",
            "resource",
            "signing",
            "--workspace",
            "hooked",
            "--name",
            "default",
        ],
        vec!["setup", "hooked"],
        vec!["rm", "--wait", "hooked", "--yes"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success(), "{output:?}");
    }
    fs::write(fixture.root.path().join("hook-continue"), "").unwrap();
    assert!(acquire.wait_with_output().unwrap().status.success());
    fixture.ok(&["rm", "--wait", "hooked", "--yes"]);
}

#[test]
fn workspace_hook_resolution_preserves_layers_and_directories() {
    let mut fixture = Fixture::with_config(Some(""));
    let workspace = fixture.add("hooks");
    let worktree = Path::new(workspace["path"].as_str().unwrap());
    // Global hooks are installed only after creation, so these paths need not
    // exist: resolution must not run a hook or require its executable yet.
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        "hooks.pre_setup = 'global'\nhooks.post_setup = 'global'\nhooks.pre_remove = 'global'\nhooks.post_remove = 'global'\n\
         hooks.post_resource_acquire = 'global'\nhooks.pre_resource_release = 'global'\n",
    )
    .unwrap();
    fixture.restart();
    let resolve = |kind: &str| {
        fixture.request_with_id(
            2,
            serde_json::json!({"workspace_hook":{"workspace":workspace["id"],"kind":kind}}),
        )
    };
    let saved = fixture.root.path().join("saved.toml");
    let save = |text: &str| {
        fs::write(&saved, text).unwrap();
        fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    };
    for (kind, global) in [
        ("setup", false),
        ("pre_setup", true),
        ("post_setup", true),
        ("pre_remove", true),
        ("post_remove", true),
        ("post_resource_acquire", true),
        ("pre_resource_release", true),
    ] {
        let key = if kind == "setup" {
            "setup.command".to_owned()
        } else {
            format!("hooks.{kind}")
        };
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

        fs::write(&file, format!("{key} = 'file hook'\n")).unwrap();
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(directory.join("file hook"))
        );
        // An unrelated saved option must not mask the worktree's hook.
        save("agent.default = 'claude'\n");
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(directory.join("file hook"))
        );
        // A relative path in the local config names a script beside it.
        save(&format!("{key} = 'saved hook'\n"));
        let local = fixture.ok(&["repo", "config", fixture.repo.to_str().unwrap()]);
        let local_directory = Path::new(local["path"].as_str().unwrap()).parent().unwrap();
        assert_eq!(
            resolve(kind)["data"],
            serde_json::json!(local_directory.join("saved hook"))
        );
        save(&format!("{key} = '/absolute/hook'\n"));
        assert_eq!(resolve(kind)["data"], "/absolute/hook");
    }
}

#[test]
fn global_post_setup_and_pre_remove_hooks_run_without_setup() {
    let fixture = Fixture::with_config(Some(
        "hooks.post_setup = 'hook.sh'\nhooks.pre_remove = 'hook.sh'\n[env]\nSESSION = '{workspace}'\nPROFILE = '{workspace_path}/profile'\n",
    ));
    install_test_script(
        fixture.repo.join("hook.sh"),
        r#"#!/bin/sh
set -eu
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test -f tracked
test "$SESSION" = "$SHOAL_WORKSPACE"
test "$PROFILE" = "$SHOAL_WORKSPACE_PATH/profile"
printf '%s\n' "$SHOAL_HOOK" >> "$HOME/hooks"
"#,
    );
    commit_resource_config(&fixture.repo, "");
    let workspace = fixture.add("global-hooks");
    assert_eq!(workspace["state"], "ready");
    let markers = fixture.root.path().join("hooks");
    assert_eq!(fs::read_to_string(&markers).unwrap(), "post_setup\n");
    fixture.ok(&["rm", "--wait", "global-hooks", "--yes"]);
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert_eq!(
        fs::read_to_string(&markers).unwrap(),
        "post_setup\npre_remove\n"
    );
}

#[test]
fn pre_setup_hook_gates_readiness_and_supports_global_defaults() {
    let fixture = Fixture::with_config(Some("hooks.pre_setup = 'before.sh'\n"));
    install_test_script(
        fixture.repo.join("before.sh"),
        r#"#!/bin/sh
set -eu
test "$SHOAL_HOOK" = pre_setup
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
echo before >> "$HOME/setup-order"
test ! -f "$HOME/fail-before" || { echo 'prepare failed' >&2; exit 7; }
"#,
    );
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\necho setup >> \"$HOME/setup-order\"\n",
    );
    commit_resource_config(&fixture.repo, "setup.command = 'setup.sh'\n");
    let home = fixture.root.path();
    fs::write(home.join("fail-before"), "").unwrap();
    let output = fixture.run(&["start", fixture.repo.to_str().unwrap(), "hooked"]);
    assert!(!output.status.success());
    let inspection = fixture.ok(&["status", "hooked"]);
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
        "hooks.pre_setup = '/usr/bin/true'\n",
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
    fixture.ok(&["rm", "--wait", "hooked", "--yes"]);
    fixture.ok(&["rm", "--wait", "hook-only", "--yes"]);
}

#[test]
fn post_remove_hook_uses_checkout_and_reports_failure_after_removal() {
    let fixture = Fixture::with_config(Some(
        "hooks.post_remove = '/usr/bin/false'\n[env]\nPROFILE = '{workspace_path}/global-profile'\n",
    ));
    install_test_script(
        fixture.repo.join("after removal.sh"),
        r#"#!/bin/sh
set -eu
test "$SHOAL_HOOK" = post_remove
test -z "${SHOAL_SCOPE_TOKEN:-}"
test -z "${SHOAL_EXECUTION_ID:-}"
test ! -e "$SHOAL_WORKSPACE_PATH"
if [ "$SHOAL_WORKSPACE" = hooked ]; then
  test "$PROFILE" = "$SHOAL_WORKSPACE_PATH/saved-profile"
else
  test "$PROFILE" = "$SHOAL_WORKSPACE_PATH/global-profile"
fi
printf '%s\n%s\n%s\n' "$PWD" "$SHOAL_WORKSPACE" "$SHOAL_WORKSPACE_PATH" >> "$HOME/removed"
test ! -f "$HOME/fail-after" || { echo 'external cleanup failed' >&2; exit 8; }
"#,
    );
    commit_resource_config(
        &fixture.repo,
        "hooks.post_remove = 'after removal.sh'\n[resources.signing]\n",
    );
    let workspace = fixture.add("hooked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fixture.ok(&["acquire", "resource", "signing", "--workspace", "hooked"]);
    let config = path.join(".shoal.toml");
    fs::write(
        &config,
        "hooks.post_remove = 'after removal.sh'\nhooks.pre_remove = '/usr/bin/false'\n",
    )
    .unwrap();
    assert!(
        !fixture
            .run(&["rm", "--wait", "hooked", "--yes"])
            .status
            .success()
    );
    assert!(!fixture.root.path().join("removed").exists());
    fs::write(
        &config,
        "hooks.post_remove = 'after removal.sh'\n[env]\nPROFILE = '{workspace_path}/saved-profile'\n",
    )
    .unwrap();
    fs::write(fixture.root.path().join("fail-after"), "").unwrap();
    let result = fixture.ok(&["rm", "--wait", "hooked", "--yes"]);
    assert_eq!(result["removed"], true);
    assert!(
        result["hook_error"]
            .as_str()
            .unwrap()
            .contains("external cleanup failed")
    );
    assert!(!fixture.run(&["status", "hooked"]).status.success());
    let notifications = fixture.ok(&["alerts"]);
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
    fixture.ok(&["acquire", "resource", "signing", "--workspace", "next"]);
    let missing = fixture.add("missing");
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    fixture.ok(&["rm", "--wait", "missing", "--yes"]);
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
