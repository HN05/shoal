use crate::support::{Fixture, commit_resource_config, wait_until};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

#[test]
fn execution_preserves_pipes_exit_code_environment_and_current_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.add("execute");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::create_dir(path.join("nested")).unwrap();
    let mut child = fixture
        .command()
        .current_dir(path.join("nested"))
        .args([
            "exec",
            "--",
            "sh",
            "-c",
            "cat; printf '%s' \"$SHOAL_WORKSPACE\"; printf error >&2; exit 7",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"pipe:").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"pipe:execute");
    assert_eq!(output.stderr, b"error");
    assert_eq!(
        fixture.ok(&["inspect", "execute"])["executions"],
        serde_json::json!([])
    );
    fixture.ok(&["rm", "execute"]);
}

#[test]
fn stop_and_manual_removal_terminate_connected_executions() {
    let fixture = Fixture::new();
    for operation in ["stop", "rm"] {
        fixture.add("running");
        let child = fixture
            .command()
            .args(["exec", "running", "--", "sleep", "5"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while fixture.ok(&["inspect", "running"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        fixture.ok(&[operation, "running"]);
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(Instant::now() < deadline);
        if operation == "stop" {
            fixture.ok(&["rm", "running"]);
        }
    }
}

#[test]
fn execution_scope_limits_management_and_expires() {
    let fixture = Fixture::new();
    let setup = fixture.repo.join("setup.sh");
    fs::write(&setup, "#!/bin/sh\nprintf setup >> setup-runs\n").unwrap();
    fs::set_permissions(&setup, fs::Permissions::from_mode(0o755)).unwrap();
    let post_setup = fixture.repo.join("post-setup.sh");
    fs::write(
        &post_setup,
        "#!/bin/sh\ntest -z \"$SHOAL_SCOPE_TOKEN\" || exit 81\ntest -z \"$SHOAL_EXECUTION_ID\" || exit 82\ntest -z \"$SHOAL_RESERVED_PORT_ENV\" || exit 83\ntest -z \"$SHOAL_PORT_WEB\" || exit 84\nprintf hook >> hook-runs\nsleep 30 < /dev/null > /dev/null 2>&1 &\n",
    )
    .unwrap();
    fs::set_permissions(&post_setup, fs::Permissions::from_mode(0o755)).unwrap();
    commit_resource_config(
        &fixture.repo,
        "setup_cmd = 'setup.sh'\npost_setup_cmd = 'post-setup.sh'\n",
    );
    let worker = fixture.add("worker");
    let worker_path = Path::new(worker["path"].as_str().unwrap());
    fixture.add("other");
    let binary = env!("CARGO_BIN_EXE_shoal");
    let scoped = |args: &[&str]| {
        fixture
            .command()
            .args(["exec", "worker", "--", binary])
            .args(args)
            .output()
            .unwrap()
    };
    let output = scoped(&["--json", "list"]);
    assert!(output.status.success());
    let list: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "worker");
    let output = scoped(&["--json", "config", "show"]);
    assert!(output.status.success(), "{output:?}");
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        config
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["key"] == "setup_cmd" && entry["value"] == "setup.sh" })
    );
    assert!(scoped(&["port", "acquire", "web"]).status.success());
    let sibling_started = worker_path.join("sibling-started");
    let mut sibling = fixture
        .command()
        .args([
            "exec",
            "worker",
            "--",
            "sh",
            "-c",
            "touch sibling-started; sleep 30",
        ])
        .spawn()
        .unwrap();
    wait_until("sibling execution", || sibling_started.exists());
    let output = scoped(&["setup", "worker"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("active or unknown execution"),
        "{output:?}"
    );
    fixture.ok(&["stop", "worker"]);
    assert!(!sibling.wait().unwrap().success());
    let output = scoped(&["setup", "worker"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(worker_path.join("setup-runs")).unwrap(),
        "setupsetup"
    );
    assert_eq!(
        fs::read_to_string(worker_path.join("hook-runs")).unwrap(),
        "hookhook"
    );
    assert_eq!(
        fixture.ok(&["inspect", "worker"])["executions"],
        serde_json::json!([]),
        "hook survivors must not belong to the invoking execution"
    );
    let output = scoped(&["install", "--dry-run"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot administer Shoal"));
    assert!(
        scoped(&["exec", "worker", "--", binary, "port"])
            .status
            .success()
    );
    for args in [
        vec!["rm", "worker", "--yes", "--delete-branch"],
        vec!["stop", "worker"],
        vec!["inspect", "other"],
        vec!["exec", "other", "--", "true"],
        vec!["setup", "other"],
        vec!["pr", "merged", "other"],
        vec!["pr", "clear", "other"],
        vec!["config", "show", "other"],
        vec![
            "config",
            "set",
            "default_agent",
            "codex",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ],
        vec![
            "config",
            "unset",
            "default_agent",
            "--repo",
            fixture.repo.to_str().unwrap(),
        ],
        vec!["port", "acquire", "web", "other"],
        vec!["repo", "rename", fixture.repo.to_str().unwrap(), "changed"],
        vec!["repo", "config", fixture.repo.to_str().unwrap()],
        vec!["repo", "config", fixture.repo.to_str().unwrap(), "--clear"],
        vec!["daemon", "stop"],
    ] {
        let output = scoped(&args);
        assert!(
            !output.status.success(),
            "scoped {args:?} unexpectedly succeeded"
        );
    }
    let token = fixture.run(&[
        "exec",
        "worker",
        "--",
        "sh",
        "-c",
        "printf '%s' \"$SHOAL_SCOPE_TOKEN\"",
    ]);
    let token = String::from_utf8(token.stdout).unwrap();
    assert!(!token.is_empty());
    let output = fixture
        .command()
        .env("SHOAL_SCOPE_TOKEN", token)
        .args(["list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expired or unknown"));
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 2);
}

#[test]
fn configured_commands_preserve_arguments_scope_and_exit_status() {
    let fixture = Fixture::with_config(Some(
        "[commands]\ncheck = ['sh', '-c', 'cat; printf \"%s\\n\" \"$SHOAL_WORKSPACE\" \"$@\"; test -n \"$SHOAL_SCOPE_TOKEN\" || exit 99; exit 7', 'check', 'literal $HOME']\n",
    ));
    let workspace = fixture.ok(&["add", fixture.repo.to_str().unwrap(), "custom"]);
    let path = workspace["path"].as_str().unwrap();
    let mut child = fixture
        .command()
        .current_dir(path)
        .args(["check", "--", "two words", "--flag"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"pipe:").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"pipe:custom\nliteral $HOME\ntwo words\n--flag\n"
    );
    assert_eq!(
        fixture.ok(&["inspect", "custom"])["executions"],
        serde_json::json!([])
    );
    fs::write(
        Path::new(path).join(".shoal.toml"),
        "[commands]\ncheck = ['printf', '%s', 'from worktree']\n",
    )
    .unwrap();
    assert_eq!(fixture.run(&["check", "custom"]).stdout, b"from worktree");
    let saved = fixture.root.path().join("commands.toml");
    fs::write(&saved, "[commands]\ncheck = ['printf', '%s', 'saved']\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    assert_eq!(fixture.run(&["check", "custom"]).stdout, b"saved");
}

#[test]
fn run_lists_command_layers_and_executes_names_that_collide_with_built_ins() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nglobal = ['printf', '%s', 'global command']\nshadowed = ['global']\nlist = ['printf', '%s', 'configured list']\nclaude = ['global-claude']\n",
    ));
    let workspace = fixture.add("run-command");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nshadowed = ['worktree']\nworktree = ['worktree-only']\n",
    )
    .unwrap();
    let saved = fixture.root.path().join("run-commands.toml");
    fs::write(
        &saved,
        "[commands]\nshadowed = ['saved']\nsaved = ['saved-only']\n",
    )
    .unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);

    let output = fixture.run(&["run", "list", "run-command"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"configured list");
    assert!(fixture.ok(&["list"]).is_array());

    let output = fixture
        .command()
        .current_dir(path)
        .args(["--json", "run"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    let command = |name: &str| listed.iter().find(|entry| entry["name"] == name).unwrap();
    assert_eq!(command("codex")["layer"], "built_in_default");
    assert_eq!(command("codex")["bare_name"], "built_in");
    assert_eq!(command("claude")["layer"], "global_config");
    assert_eq!(command("global")["bare_name"], "shorthand");
    assert_eq!(command("list")["bare_name"], "built_in");
    assert_eq!(command("worktree")["layer"], "worktree_file");
    assert_eq!(command("saved")["layer"], "saved_repository_config");
    assert_eq!(command("shadowed")["argv"], serde_json::json!(["saved"]));
    assert_eq!(command("shadowed")["layer"], "saved_repository_config");
}

#[test]
fn unknown_commands_never_open_the_workspace_picker() {
    let fixture = Fixture::new();
    let workspace = fixture.add("command-target");
    // Noninteractive selection used to fail at the picker instead of name resolution.
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["lsit"])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        error.contains("lsit") && error.contains("no current workspace"),
        "{error}"
    );
    assert!(!error.contains("non-interactive"), "{error}");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nlocal-check = ['printf', '%s', 'repository command']\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .current_dir(path)
        .arg("local-check")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"repository command");
    assert_eq!(
        fixture.run(&["local-check", "command-target"]).stdout,
        b"repository command"
    );
    let output = fixture
        .command()
        .current_dir(path)
        .arg("lsit")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command"));
}
