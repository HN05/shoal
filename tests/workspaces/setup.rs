use super::recovery::recovery_report;
use crate::support::{Fixture, commit_resource_config, git};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

#[test]
fn setup_commands_cannot_recursively_run_setup() {
    let fixture = Fixture::new();
    let setup = fixture.repo.join("setup.sh");
    fs::write(&setup, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&setup, fs::Permissions::from_mode(0o755)).unwrap();
    commit_resource_config(&fixture.repo, "setup_cmd = 'setup.sh'\n");
    let worker = fixture.add("worker");
    let setup = Path::new(worker["path"].as_str().unwrap()).join("setup.sh");
    fs::write(
        &setup,
        format!("#!/bin/sh\n'{}' setup\n", env!("CARGO_BIN_EXE_shoal")),
    )
    .unwrap();

    let output = fixture.run(&["setup", "worker"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("a setup command cannot recursively run setup"),
        "{output:?}"
    );
}

#[test]
fn setup_cmd_resolves_worktree_paths_and_runs_before_agents() {
    let fixture = Fixture::new();
    let script = "setup 'literal' $name.sh";
    fs::write(
        fixture.repo.join(script),
        r#"#!/bin/sh
printf 'setup output\n'
printf '%s' "$PWD" > setup-cwd
printf '%s' "$SHOAL_WORKSPACE" > setup-workspace
"$SHOAL_TEST_BIN" --json status > during-setup.json || exit 91
"#,
    )
    .unwrap();
    fs::set_permissions(fixture.repo.join(script), fs::Permissions::from_mode(0o755)).unwrap();
    commit_resource_config(&fixture.repo, &format!("setup_cmd = {script:?}\n"));
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("codex"),
        "#!/bin/sh\ntest -f setup-cwd || exit 92\nprintf agent > agent-started\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "prepared",
            "--agent",
            "codex",
        ])
        .env("SHOAL_TEST_BIN", env!("CARGO_BIN_EXE_shoal"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(workspace["state"], "ready");
    assert!(String::from_utf8_lossy(&output.stderr).contains("setup output"));
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("setup-cwd")).unwrap(),
        fs::canonicalize(path).unwrap().to_str().unwrap()
    );
    assert_eq!(
        fs::read_to_string(path.join("setup-workspace")).unwrap(),
        "prepared"
    );
    assert!(path.join("agent-started").exists());
    assert!(!fixture.repo.join("setup-cwd").exists());
    let during: Value =
        serde_json::from_slice(&fs::read(path.join("during-setup.json")).unwrap()).unwrap();
    assert_eq!(during["workspace"]["state"], "preparing");
    assert_eq!(during["setup_finished"], false);
    assert_eq!(during["executions"].as_array().unwrap().len(), 1);
}

#[test]
fn setup_cmd_local_override_absolute_path_failure_and_retry() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "setup_cmd = 'must-not-run'\n");
    let script = fixture.root.path().join("system setup.sh");
    fs::write(
        &script,
        "#!/bin/sh\nprintf partial > setup-result\nexit 17\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let input = fixture.root.path().join("local.toml");
    fs::write(
        &input,
        format!("setup_cmd = {:?}\n", script.to_str().unwrap()),
    )
    .unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        input.to_str().unwrap(),
    ]);
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("claude"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture.run(&[
        "--json",
        "add",
        fixture.repo.to_str().unwrap(),
        "failed-setup",
        "--agent",
        "claude",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("status 17"));
    let failed = fixture.ok(&["inspect", "failed-setup"]);
    assert_eq!(failed["workspace"]["state"], "failed");
    assert_eq!(failed["executions"], serde_json::json!([]));
    let report = recovery_report(&fixture, &["doctor", "failed-setup"]);
    assert_eq!(report[0]["directory"], "valid");
    let issues = report[0]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    let issue = issues[0].as_str().unwrap();
    assert!(issue.contains("setup failed (exit 17"));
    assert!(issue.contains("retry with shoal setup"));
    assert!(issue.contains("alternatively, use --repair"));
    let output = fixture.run(&["doctor", "failed-setup"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stdout).contains(issue));
    assert_eq!(
        fixture.ok(&["inspect", "failed-setup"])["workspace"],
        failed["workspace"]
    );
    assert_eq!(
        fixture.ok(&["status", "failed-setup"])["setup_finished"],
        false
    );
    let path = Path::new(failed["workspace"]["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("setup-result")).unwrap(),
        "partial"
    );
    assert!(!path.join("agent-started").exists());
    assert!(
        !fixture
            .run(&["exec", "failed-setup", "--", "true"])
            .status
            .success()
    );
    let output = fixture.run(&["prepare", "failed-setup"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command \"prepare\""));
    fs::write(&script, "#!/bin/sh\nprintf complete > setup-result\n").unwrap();
    assert_eq!(fixture.ok(&["setup", "failed-setup"])["state"], "ready");
    assert_eq!(
        fixture.ok(&["status", "failed-setup"])["setup_finished"],
        true
    );
    assert_eq!(
        fs::read_to_string(path.join("setup-result")).unwrap(),
        "complete"
    );
}

#[test]
fn setup_failure_prompt_ignores_or_deletes_only_new_workspace() {
    let fixture = Fixture::new();
    fixture.add("existing");
    commit_resource_config(&fixture.repo, "setup_cmd = 'missing-script'\n");
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("codex"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let config = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        "[codex]\ndefault_mode = 'app'\n",
    )
    .unwrap();
    let (ignored, transcript) = fixture.interactive(
        &[
            "add",
            fixture.repo.to_str().unwrap(),
            "ignored",
            "--agent",
            "codex",
        ],
        "i\n",
    );
    assert!(ignored.status.success(), "{transcript}");
    assert!(transcript.contains("[d]elete workspace"));
    assert_eq!(
        fixture.ok(&["inspect", "ignored"])["workspace"]["state"],
        "ready"
    );
    assert_eq!(fixture.ok(&["status", "ignored"])["setup_finished"], false);
    let ignored_path = fixture.ok(&["inspect", "ignored"])["workspace"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(Path::new(&ignored_path).join("agent-started").exists());
    let (deleted, transcript) = fixture.interactive(
        &["add", fixture.repo.to_str().unwrap(), "deleted"],
        "d\ny\n",
    );
    assert!(!deleted.status.success(), "{transcript}");
    assert!(
        transcript.contains("Deleted workspace deleted"),
        "{transcript}"
    );
    assert!(!fixture.run(&["inspect", "deleted"]).status.success());
    assert!(
        git(&fixture.repo, &["branch", "--list", "deleted"])
            .trim()
            .is_empty()
    );
    assert!(fixture.repo.is_dir());
    assert_eq!(
        fixture.ok(&["inspect", "existing"])["workspace"]["state"],
        "ready"
    );
    let (canceled, transcript) =
        fixture.interactive(&["add", fixture.repo.to_str().unwrap(), "canceled"], "\n");
    assert!(!canceled.status.success(), "{transcript}");
    assert_eq!(
        fixture.ok(&["inspect", "canceled"])["workspace"]["state"],
        "failed"
    );
}

#[test]
fn setup_interruption_preserves_work_and_blocks_concurrent_execution() {
    let mut fixture = Fixture::new();
    fs::write(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\nprintf partial > setup-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::set_permissions(
        fixture.repo.join("setup.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_resource_config(&fixture.repo, "setup_cmd = 'setup.sh'\n");
    let mut add = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "interrupted",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let path = fixture
        .shoal_dir()
        .join("repo-with---quotes----literal/interrupted");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.join("setup-started").exists() {
        assert!(Instant::now() < deadline, "setup did not start");
        assert!(add.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "preparing"
    );
    assert!(
        !fixture
            .run(&["exec", "interrupted", "--", "true"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["--json", "setup", "interrupted"])
            .status
            .success()
    );
    // Signal the wrapper so it can stop its recorded process group and report failure.
    assert_eq!(unsafe { libc::kill(add.id() as i32, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = add.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        assert!(Instant::now() < deadline, "setup did not stop");
        thread::sleep(Duration::from_millis(20));
    }
    let inspection = fixture.ok(&["inspect", "interrupted"]);
    assert_eq!(inspection["workspace"]["state"], "failed");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "interrupted"])["workspace"]["state"],
        "failed"
    );
    assert!(path.join("setup-started").exists());
    fixture.ok(&["rm", "interrupted", "--yes", "--delete-branch"]);
    assert!(!path.exists());
}
