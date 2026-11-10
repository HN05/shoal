//! Creating workspaces: `shoal add` and `split`, their bases and issues, and
//! setup and its interruptions.
use super::forge::{forge_origin, github_issue, github_open_items, install_api_curl, script_api};
use super::recovery::recovery_report;
use super::start::strip_shoal_arguments;
use super::*;

#[test]
fn named_workspace_uses_committed_history_and_deletes_redundant_branch() {
    let fixture = Fixture::new();
    fs::write(fixture.repo.join("tracked"), "uncommitted source\n").unwrap();
    let workspace = fixture.add("fix-login");
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(path.file_name().unwrap(), "fix-login");
    assert_eq!(
        fs::read_to_string(path.join("tracked")).unwrap(),
        "committed\n"
    );
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "disposable").unwrap();
    fixture.ok(&["rm", "fix-login"]);
    assert!(!path.exists());
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/heads/{}", workspace["branch"].as_str().unwrap())
            ]
        ),
        ""
    );
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    assert_eq!(
        fs::read_to_string(fixture.repo.join("tracked")).unwrap(),
        "uncommitted source\n"
    );
}

#[test]
fn failed_creation_cannot_adopt_an_unrelated_worktree_at_its_reserved_path() {
    let fixture = Fixture::new();
    let failed = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "reserved",
        "--base",
        "does-not-exist",
    ]);
    assert!(!failed.status.success());
    let reserved = PathBuf::from(
        fixture.ok(&["status", "reserved"])["workspace"]["path"]
            .as_str()
            .unwrap(),
    );
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "unrelated",
            reserved.to_str().unwrap(),
            "main",
        ],
    );
    let report = recovery_report(&fixture, &["reserved", "--repair"]);
    assert_eq!(report[0]["directory"], "unverified");
    let removal = fixture.run(&["rm", "reserved", "--yes"]);
    assert!(!removal.status.success());
    assert!(reserved.exists());
}

#[test]
fn add_issue_takes_the_repository_of_the_current_checkout_or_workspace() {
    let fixture = Fixture::with_config(Some(
        "[agents.reviewer]\ncommand = ['printf', '%s', '{prompt}']\n",
    ));
    forge_origin(&fixture);
    let bin = fixture.root.path().join("issue-bin");
    // The daemon reads the issue through the scripted curl.
    install_api_curl(&fixture.root.path().join("bin"));
    script_api(
        fixture.root.path(),
        "repos/team/project/issues/12",
        &serde_json::json!({"number": 12, "title": "Clarify retries", "state": "open",
            "body": "", "labels": [], "pull_request": null}),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    // No terminal, as in a planning chat's tool call: no picker may run.
    let add = |cwd: &Path| {
        let output = fixture
            .command()
            .args([
                "--json", "start", "--issue", "12", "--here", "--agent", "reviewer",
            ])
            .current_dir(cwd)
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_str::<Value>(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap()
    };
    let created = add(&fixture.repo);
    assert_eq!(created["name"], "12-clarify-retries");
    let workspace = PathBuf::from(created["path"].as_str().unwrap());
    assert_eq!(add(&workspace)["id"], created["id"]);
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

/// An agent copies its own workspace into a child, on a new branch or at
/// its commit, and no workspace outside its tree.
#[test]
fn an_agent_splits_copies_of_its_own_workspace_only() {
    let fixture = Fixture::new();
    fixture.add("parent");
    fixture.add("other");
    let parent = fixture.ok(&["status", "parent"])["workspace"].clone();
    let path = PathBuf::from(parent["path"].as_str().unwrap());
    fs::write(path.join("untracked"), "work in progress\n").unwrap();
    for (args, branch) in [
        (&["split", "--copy"][..], Some("parent-copy")),
        (&["split", "--no-branch", "--copy", "parent"], None),
    ] {
        let split = scoped_command(&fixture, "parent", args);
        assert!(split.status.success(), "{args:?}: {split:?}");
        let child: Value = serde_json::from_slice(&split.stdout).unwrap();
        assert_eq!(child["branch"].as_str(), branch, "{child}");
        assert_eq!(
            child["copied_from"]["workspace"]["id"], parent["id"],
            "{child}"
        );
        assert_eq!(child["parent"]["id"], parent["id"], "{child}");
        let copied = Path::new(child["path"].as_str().unwrap()).join("untracked");
        assert_eq!(fs::read_to_string(copied).unwrap(), "work in progress\n");
    }
    let refused = scoped_command(&fixture, "parent", &["split", "--copy", "other"]);
    assert!(!refused.status.success(), "{refused:?}");
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 4);
}

#[test]
fn claude_worktree_hooks_split_and_finish_a_child_workspace() {
    // Completion without cleanup keeps the finished child to inspect.
    let fixture = Fixture::with_config(Some("[cleanup.done]\nenabled = false\n"));
    let parent = fixture.add("parent");
    let hook = |command: &str, input: Value| scoped_hook(&fixture, "parent", command, input);
    let created = hook(
        "worktree-create",
        serde_json::json!({
            "hook_event_name": "WorktreeCreate",
            "name": "agent-a1b2",
            "cwd": parent["path"],
        }),
    );
    // Claude Code reads the last line as the worktree's path.
    let path = created.lines().last().unwrap();
    let child = fixture.ok(&["status", "agent-a1b2"]);
    assert_eq!(child["workspace"]["path"], path, "{created}");
    assert_eq!(child["workspace"]["parent"]["name"], "parent");
    assert!(child["completion"].is_null());
    hook(
        "worktree-remove",
        serde_json::json!({"hook_event_name": "WorktreeRemove", "worktree_path": path}),
    );
    assert!(!fixture.ok(&["status", "agent-a1b2"])["completion"].is_null());
    // A child its subagent already finished is torn down as it is.
    hook(
        "worktree-remove",
        serde_json::json!({"hook_event_name": "WorktreeRemove", "worktree_path": path}),
    );
    // A worktree Shoal already removed has nothing left to finish.
    hook(
        "worktree-remove",
        serde_json::json!({"worktree_path": parent["path"].as_str().unwrap().to_owned() + "-gone"}),
    );
}

#[test]
fn setup_commands_cannot_recursively_run_setup() {
    let fixture = Fixture::new();
    let setup = fixture.repo.join("setup.sh");
    install_test_script(&setup, "#!/bin/sh\nexit 0\n");
    commit_resource_config(&fixture.repo, "setup.command = 'setup.sh'\n");
    let worker = fixture.add("worker");
    let setup = Path::new(worker["path"].as_str().unwrap()).join("setup.sh");
    fs::write(
        &setup,
        format!("#!/bin/sh\n'{}' setup\n", support::shoal_binary()),
    )
    .unwrap();

    let output = fixture.run(&["setup", "worker"]);
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn setup_continues_after_the_cli_that_started_it_exits() {
    let fixture = Fixture::new();
    let release = fixture.root.path().join("release-setup");
    install_test_script(
        fixture.repo.join("setup.sh"),
        format!(
            "#!/bin/sh\necho preparing\ntouch setup-started\nwhile [ ! -e '{}' ]; do sleep 0.1; done\ntouch setup-finished\n",
            release.display()
        ),
    );
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        "#!/bin/sh\ntouch post-setup-ran\n",
    );
    commit_resource_config(
        &fixture.repo,
        "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\n",
    );
    let repo = fixture.repo.to_str().unwrap();
    // The CLI shows what setup writes, as it did when it ran setup itself.
    fs::write(&release, "").unwrap();
    let output = fixture.run(&["--json", "start", repo, "quick"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("preparing"),
        "{output:?}"
    );
    fs::remove_file(&release).unwrap();
    let mut add = fixture
        .command()
        .args(["--json", "start", repo, "slow"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let path = wait_for("the slow workspace", || {
        let listed = fixture.ok(&["list"]);
        let slow = listed.as_array()?.iter().find(|w| w["name"] == "slow")?;
        Some(PathBuf::from(slow["path"].as_str()?))
    });
    wait_until("setup to start", || path.join("setup-started").exists());
    add.kill().unwrap();
    add.wait().unwrap();
    fs::write(&release, "").unwrap();
    // The post-setup hook runs with setup, so it runs without the CLI too.
    wait_until("the post-setup hook to run", || {
        path.join("post-setup-ran").exists()
    });
    assert!(path.join("setup-finished").exists());
    assert_eq!(
        fixture.ok(&["status", "slow"])["workspace"]["state"],
        "ready"
    );
}

#[test]
fn an_operation_keeps_its_scope_after_the_execution_that_started_it_ends() {
    let fixture = Fixture::new();
    let release = fixture.root.path().join("release-setup");
    install_test_script(
        fixture.repo.join("setup.sh"),
        format!(
            "#!/bin/sh\ntouch setup-started\nwhile [ ! -e '{}' ]; do sleep 0.1; done\n",
            release.display()
        ),
    );
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        "#!/bin/sh\ntouch post-setup-ran\n",
    );
    commit_resource_config(
        &fixture.repo,
        "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\n",
    );
    fs::write(&release, "").unwrap();
    let worker = fixture.add("worker");
    let path = PathBuf::from(worker["path"].as_str().unwrap());
    fs::remove_file(&release).unwrap();
    fs::remove_file(path.join("setup-started")).unwrap();
    fs::remove_file(path.join("post-setup-ran")).unwrap();
    // A scoped command reruns setup, drops its CLI and ends normally, so its
    // execution's end stops whatever still carries its identity.
    let command = format!(
        "'{}' setup worker >/dev/null 2>&1 & cli=$!; while [ ! -e '{}' ]; do sleep 0.1; done; kill -9 $cli",
        support::shoal_binary(),
        path.join("setup-started").display()
    );
    let output = fixture.run(&["run", "--workspace", "worker", "--", "sh", "-c", &command]);
    assert!(output.status.success(), "{output:?}");
    fs::write(&release, "").unwrap();
    wait_until("the post-setup hook to run", || {
        path.join("post-setup-ran").exists()
    });
}

#[test]
fn interrupting_the_cli_during_the_pre_setup_hook_stops_setup_as_it_starts() {
    let fixture = Fixture::new();
    let started = fixture.root.path().join("pre-setup-started");
    let release = fixture.root.path().join("release-pre-setup");
    install_test_script(
        fixture.repo.join("pre-setup.sh"),
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.1; done\n",
            started.display(),
            release.display()
        ),
    );
    // Setup that ran on would outlast every wait below.
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\nexec python3 -c 'import time; time.sleep(600)'\n",
    );
    commit_resource_config(
        &fixture.repo,
        "setup.command = 'setup.sh'\nhooks.pre_setup = 'pre-setup.sh'\n",
    );
    let mut add = fixture
        .command()
        .args(["--json", "start", fixture.repo.to_str().unwrap(), "gated"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = lines(add.stderr.take().unwrap());
    wait_until("the pre-setup hook to start", || started.exists());
    assert_eq!(unsafe { libc::kill(add.id() as i32, libc::SIGTERM) }, 0);
    // The worker says so once it is cancelled, before its setup listens for
    // signals.
    wait_until("the cancel to reach the worker", || {
        stderr
            .try_iter()
            .any(|line| line == "Operation cancelled; stopping its work")
    });
    fs::write(&release, "").unwrap();
    let status = wait_for("the CLI to stop", || add.try_wait().unwrap());
    assert!(!status.success());
    let status = fixture.ok(&["status", "gated"]);
    assert_eq!(status["workspace"]["state"], "failed");
    assert_eq!(status["executions"], serde_json::json!([]));
}

#[test]
fn interrupting_the_cli_stops_the_post_setup_hook_and_what_ignores_sigterm_below_it() {
    let fixture = Fixture::new();
    let pid_file = fixture.root.path().join("hook.pid");
    // The hook exits on SIGTERM; what it started does not.
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        format!(
            "#!/bin/sh\nsh -c 'trap \"\" TERM; echo $$ > \"$0\"; exec python3 -c \"import time; time.sleep(60)\"' '{}' &\nwait\n",
            pid_file.display()
        ),
    );
    commit_resource_config(&fixture.repo, "hooks.post_setup = 'post-setup.sh'\n");
    let mut add = fixture
        .command()
        .args(["--json", "start", fixture.repo.to_str().unwrap(), "hooked"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid: u32 = wait_for("the hook to start", || {
        fs::read_to_string(&pid_file).ok()?.trim().parse().ok()
    });
    assert_eq!(unsafe { libc::kill(add.id() as i32, libc::SIGTERM) }, 0);
    let status = wait_for("the CLI to stop", || add.try_wait().unwrap());
    assert!(!status.success());
    wait_until("what the hook started to stop", || !process_running(pid));
    assert_eq!(
        fixture.ok(&["status", "hooked"])["workspace"]["state"],
        "ready"
    );
}

#[test]
fn add_uses_develop_as_the_repository_default() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["branch", "-m", "develop"]);
    let author = upstream_remote(&fixture);
    let expected = git(&author, &["rev-parse", "HEAD"]);
    fixture.ok(&["repo", "sync", fixture.repo.to_str().unwrap()]);
    let workspace = fixture.add("henrik/8374-set-league-season-player-profile");
    assert_eq!(workspace["base_ref"], "refs/heads/develop");
    assert_eq!(workspace["base_commit"], expected.trim());
    let name = workspace["name"].as_str().unwrap();
    assert_eq!(fixture.ok(&["rm", name])["branch_deleted"], true);
}

#[test]
fn add_base_accepts_branches_tags_commits_and_revision_expressions() {
    let fixture = Fixture::new();
    let initial = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(&fixture.repo, &["checkout", "-b", "feature/source"]);
    fs::write(fixture.repo.join("source-only"), "base branch content\n").unwrap();
    git(&fixture.repo, &["add", "."]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "source change",
        ],
    );
    let source = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "tag",
            "-a",
            "v1",
            "-m",
            "release",
        ],
    );
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/topic", source.trim()],
    );
    git(&fixture.repo, &["checkout", "main"]);
    // Explicit non-default bases must work even when the default cannot refresh.
    git(
        &fixture.repo,
        &["remote", "add", "origin", "/nonexistent/shoal-test-remote"],
    );
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    for (index, (base, expected, reference)) in [
        (
            "feature/source",
            source.trim(),
            Some("refs/heads/feature/source"),
        ),
        (
            "refs/heads/feature/source",
            source.trim(),
            Some("refs/heads/feature/source"),
        ),
        ("v1", source.trim(), Some("refs/tags/v1")),
        (source.trim(), source.trim(), None),
        ("feature/source~1", initial.trim(), None),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("from-base-{index}");
        let workspace = fixture.ok(&[
            "start",
            fixture.repo.to_str().unwrap(),
            &name,
            "--base",
            base,
        ]);
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(git(path, &["rev-parse", "HEAD"]).trim(), expected);
        assert_eq!(workspace["base_commit"], expected);
        assert_eq!(workspace["base_ref"], serde_json::json!(reference));
        fs::write(path.join("tracked"), "workspace change\n").unwrap();
        let output = fixture.run(&["diff", &name]);
        assert!(output.status.success(), "{output:?}");
        let diff = String::from_utf8(output.stdout).unwrap();
        assert!(diff.contains("workspace change"));
        assert!(!diff.contains("source-only"));
    }
    // A remote branch base is fetched first, so an unreachable remote stops
    // creation instead of starting from a stale tracking ref.
    let output = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "from-remote",
        "--base",
        "origin/topic",
    ]);
    assert!(!output.status.success());
    assert_eq!(
        fixture.ok(&["status", "from-remote"])["workspace"]["state"],
        "failed"
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), initial);
    assert_eq!(git(&fixture.repo, &["rev-parse", "feature/source"]), source);
}

#[test]
fn add_fetches_a_remote_branch_base_before_creating_the_worktree() {
    let fixture = Fixture::new();
    let author = upstream_remote(&fixture);
    // Synced first, so the daemon's own syncs leave main where it is.
    fixture.ok(&["repo", "sync", fixture.repo.to_str().unwrap()]);
    let main = git(&fixture.repo, &["rev-parse", "main"]);
    git(&author, &["switch", "-c", "feature/api"]);
    for (index, contents) in ["first\n", "second\n"].into_iter().enumerate() {
        commit_file(&author, "api", contents);
        git(&author, &["push", "origin", "feature/api"]);
        let expected = git(&author, &["rev-parse", "HEAD"]);
        let name = format!("on-api-{index}");
        let workspace = fixture.ok(&[
            "start",
            fixture.repo.to_str().unwrap(),
            &name,
            "--base",
            "origin/feature/api",
        ]);
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(git(path, &["rev-parse", "HEAD"]), expected);
        assert_eq!(workspace["base_ref"], "refs/remotes/origin/feature/api");
        assert_eq!(workspace["base_commit"], expected.trim());
    }
    // Only the named branch is fetched; the default branch stays as it was.
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), main);
}

#[test]
fn setup_cmd_resolves_worktree_paths_and_runs_before_agents() {
    let fixture = Fixture::new();
    let script = "setup 'literal' $name.sh";
    install_test_script(
        fixture.repo.join(script),
        r#"#!/bin/sh
printf 'setup output\n'
printf '%s' "$PWD" > setup-cwd
printf '%s' "$SHOAL_WORKSPACE" > setup-workspace
"$SHOAL_TEST_BIN" --json status > during-setup.json || exit 91
"#,
    );
    commit_resource_config(&fixture.repo, &format!("setup.command = {script:?}\n"));
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("codex"),
        "#!/bin/sh\ntest -f setup-cwd || exit 92\nprintf agent > agent-started\n",
    );
    let output = fixture
        .command()
        .args([
            "--json",
            "start",
            fixture.repo.to_str().unwrap(),
            "prepared",
            "--here",
            "--agent",
            "codex",
        ])
        .env("SHOAL_TEST_BIN", support::shoal_binary())
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
fn ignored_files_are_copied_before_setup_unless_turned_off() {
    let fixture = Fixture::new();
    fs::write(fixture.repo.join(".gitignore"), ".env\nlocal/\n").unwrap();
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\ncat .env local/signing > setup-saw\n",
    );
    commit_resource_config(&fixture.repo, "setup.command = 'setup.sh'\n");
    fs::write(fixture.repo.join(".env"), "TOKEN=1\n").unwrap();
    fs::create_dir(fixture.repo.join("local")).unwrap();
    fs::write(fixture.repo.join("local/signing"), "team\n").unwrap();
    let workspace = fixture.ok(&["start", fixture.repo.to_str().unwrap(), "copied"]);
    assert_eq!(workspace["state"], "ready");
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("setup-saw")).unwrap(),
        "TOKEN=1\nteam\n"
    );

    // Rerunning setup keeps files the workspace already has.
    fs::write(path.join(".env"), "TOKEN=2\n").unwrap();
    assert_eq!(fixture.ok(&["setup", "copied"])["state"], "ready");
    assert_eq!(fs::read_to_string(path.join(".env")).unwrap(), "TOKEN=2\n");

    commit_resource_config(&fixture.repo, "setup.copy_ignored = false\n");
    let workspace = fixture.ok(&["start", fixture.repo.to_str().unwrap(), "uncopied"]);
    assert_eq!(workspace["state"], "ready");
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert!(!path.join(".env").exists());
}

#[test]
fn work_setup_skip_accepts_a_failed_setup() {
    let fixture = Fixture::new();
    let script = fixture.root.path().join("setup.sh");
    install_test_script(
        &script,
        "#!/bin/sh\nprintf partial > setup-result\nexit 17\n",
    );
    let post_setup = fixture.root.path().join("post-setup.sh");
    install_test_script(&post_setup, "#!/bin/sh\nprintf done > post-setup\n");
    let input = fixture.root.path().join("local.toml");
    fs::write(
        &input,
        format!(
            "setup.command = {:?}\nhooks.post_setup = {:?}\n",
            script.to_str().unwrap(),
            post_setup.to_str().unwrap()
        ),
    )
    .unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &input);
    let output = fixture.run(&["start", fixture.repo.to_str().unwrap(), "skipped"]);
    assert!(!output.status.success());
    assert_eq!(
        fixture.ok(&["status", "skipped"])["workspace"]["state"],
        "failed"
    );

    // Adopting its path again reclaims ownership only; a verified worktree
    // with a failed setup stays failed.
    let path = fixture.ok(&["status", "skipped"])["workspace"]["path"].clone();
    assert!(
        !fixture
            .run(&["adopt", path.as_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["status", "skipped"])["workspace"]["state"],
        "failed"
    );
    let skipped = fixture.ok(&["setup", "skipped", "--skip"]);
    assert_eq!(skipped["state"], "ready");
    let path = Path::new(skipped["path"].as_str().unwrap());
    // Skipping keeps what setup left and does not run it again, but runs
    // the post-setup hook as a ready workspace does.
    assert_eq!(
        fs::read_to_string(path.join("setup-result")).unwrap(),
        "partial"
    );
    assert_eq!(fs::read_to_string(path.join("post-setup")).unwrap(), "done");
    let again = fixture.run(&["setup", "skipped", "--skip"]);
    assert!(!again.status.success());
}

#[test]
fn setup_cmd_local_override_absolute_path_failure_and_retry() {
    let fixture = Fixture::new();
    commit_resource_config(&fixture.repo, "setup.command = 'must-not-run'\n");
    let script = fixture.root.path().join("system setup.sh");
    install_test_script(
        &script,
        "#!/bin/sh\nprintf partial > setup-result\nexit 17\n",
    );
    let input = fixture.root.path().join("local.toml");
    fs::write(
        &input,
        format!("setup.command = {:?}\n", script.to_str().unwrap()),
    )
    .unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &input);
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("claude"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    );
    let output = fixture.run(&[
        "--json",
        "start",
        fixture.repo.to_str().unwrap(),
        "failed-setup",
        "--here",
        "--agent",
        "claude",
    ]);
    assert!(!output.status.success());
    let failed = fixture.ok(&["status", "failed-setup"]);
    assert_eq!(failed["workspace"]["state"], "failed");
    assert_eq!(failed["executions"], serde_json::json!([]));
    let report = recovery_report(&fixture, &["failed-setup"]);
    assert_eq!(report[0]["directory"], "valid");
    let issues = report[0]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    let issue = issues[0].as_str().unwrap();
    assert!(issue.contains("setup failed (exit 17"));
    assert_eq!(
        fixture.ok(&["status", "failed-setup"])["workspace"],
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
            .run(&["run", "--workspace", "failed-setup", "--", "true"])
            .status
            .success()
    );
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
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        "#!/bin/sh\nprintf hook > hook-ran\n",
    );
    commit_resource_config(
        &fixture.repo,
        "setup.command = 'missing-script'\nhooks.post_setup = 'post-setup.sh'\n",
    );
    let bin = fixture.root.path().join("bin");
    install_test_script(
        bin.join("codex"),
        "#!/bin/sh\nprintf agent > agent-started\n",
    );
    let (ignored, transcript) = fixture.interactive(
        &[
            "start",
            fixture.repo.to_str().unwrap(),
            "ignored",
            "--agent",
            "codex",
        ],
        "i\n",
    );
    assert!(ignored.status.success(), "{transcript}");
    assert_eq!(
        fixture.ok(&["status", "ignored"])["workspace"]["state"],
        "ready"
    );
    assert_eq!(fixture.ok(&["status", "ignored"])["setup_finished"], false);
    let ignored_path = fixture.ok(&["status", "ignored"])["workspace"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    // Preparation goes on past the ignored failure, hook first.
    assert!(Path::new(&ignored_path).join("hook-ran").exists());
    assert!(Path::new(&ignored_path).join("agent-started").exists());
    let (deleted, transcript) = fixture.interactive(
        &["start", fixture.repo.to_str().unwrap(), "deleted"],
        "d\ny\n",
    );
    assert!(!deleted.status.success(), "{transcript}");
    assert!(!fixture.run(&["status", "deleted"]).status.success());
    assert!(
        git(&fixture.repo, &["branch", "--list", "deleted"])
            .trim()
            .is_empty()
    );
    assert!(fixture.repo.is_dir());
    assert_eq!(
        fixture.ok(&["status", "existing"])["workspace"]["state"],
        "ready"
    );
    let (canceled, transcript) =
        fixture.interactive(&["start", fixture.repo.to_str().unwrap(), "canceled"], "\n");
    assert!(!canceled.status.success(), "{transcript}");
    assert_eq!(
        fixture.ok(&["status", "canceled"])["workspace"]["state"],
        "failed"
    );
}

#[test]
fn setup_verification_failure_reports_survivor_and_keeps_work_without_prompting() {
    for json in [false, true] {
        let fixture = Fixture::new();
        let script = fixture.root.path().join("setup-survivor");
        install_test_script(
            &script,
            r#"#!/usr/bin/env python3
import subprocess, sys
from pathlib import Path
child = subprocess.Popen([sys.executable, '-c', 'import time; print("ready", flush=True); time.sleep(600)'], start_new_session=True,
    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
assert child.stdout.readline() == b'ready\n'
Path('survivor-pid').write_text(str(child.pid))
Path('setup-result').write_text('complete')
"#,
        );
        commit_resource_config(
            &fixture.repo,
            &format!(
                "setup.command = {:?}\n[agents.pi]\ncommand = ['sh', '-c', 'touch agent-started']\n",
                script.to_str().unwrap()
            ),
        );
        let mut command = fixture.command();
        command.args([
            "start",
            fixture.repo.to_str().unwrap(),
            "survivor",
            "--agent",
            "pi",
        ]);
        if json {
            command.arg("--json");
        }
        let (output, transcript) = fixture.interactive_command(&mut command, "");
        let inspection = fixture.ok(&["status", "survivor"]);
        let path = Path::new(inspection["workspace"]["path"].as_str().unwrap());
        let pid = fs::read_to_string(path.join("survivor-pid")).unwrap();
        // Reap the test-owned survivor before making assertions about the
        // output, as the message says: stop, then skip the failed setup.
        let stopped = fixture.run(&["stop", "survivor", "--yes"]);
        let skipped = fixture.run(&["setup", "survivor", "--skip"]);
        assert!(stopped.status.success(), "{stopped:?}");
        assert!(skipped.status.success(), "{skipped:?}");
        assert_eq!(
            fixture.ok(&["status", "survivor"])["workspace"]["state"],
            "ready"
        );
        assert!(!output.status.success(), "{transcript}");
        assert_eq!(inspection["workspace"]["state"], "failed");
        assert_eq!(
            fs::read_to_string(path.join("setup-result")).unwrap(),
            "complete"
        );
        assert!(!path.join("agent-started").exists());
        // The survivor is named, and no ignore-or-delete question is asked.
        assert!(transcript.contains(&format!("PID {pid}")), "{transcript}");
        assert!(!transcript.contains("[i]gnore"), "{transcript}");
    }
}

#[test]
fn setup_interruption_preserves_work_and_blocks_concurrent_execution() {
    let mut fixture = Fixture::new();
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\nprintf partial > setup-started\nwhile :; do sleep 1; done\n",
    );
    commit_resource_config(&fixture.repo, "setup.command = 'setup.sh'\n");
    let mut add = fixture
        .command()
        .args([
            "--json",
            "start",
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
    wait_until("setup to start", || {
        if path.join("setup-started").exists() {
            return true;
        }
        assert!(add.try_wait().unwrap().is_none());
        false
    });
    assert_eq!(
        fixture.ok(&["status", "interrupted"])["workspace"]["state"],
        "preparing"
    );
    assert!(
        !fixture
            .run(&["run", "--workspace", "interrupted", "--", "true"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["--json", "setup", "interrupted"])
            .status
            .success()
    );
    // Signal the CLI, which interrupts the setup's wrapper so it can stop its
    // recorded process group and report failure.
    assert_eq!(unsafe { libc::kill(add.id() as i32, libc::SIGTERM) }, 0);
    let status = wait_for("setup to stop", || add.try_wait().unwrap());
    assert!(!status.success());
    let inspection = fixture.ok(&["status", "interrupted"]);
    assert_eq!(inspection["workspace"]["state"], "failed");
    assert_eq!(inspection["executions"], serde_json::json!([]));
    fixture.restart();
    assert_eq!(
        fixture.ok(&["status", "interrupted"])["workspace"]["state"],
        "failed"
    );
    assert!(path.join("setup-started").exists());
    fixture.ok(&["rm", "interrupted", "--yes"]);
    assert!(!path.exists());
}

#[test]
fn add_from_issue_reads_the_forge_and_passes_context_to_agents() {
    let fixture = Fixture::new();
    let bin = fixture.root.path().join("issue-bin");
    // The daemon reads the issue through the scripted curl.
    install_api_curl(&fixture.root.path().join("bin"));
    for tool in ["codex", "claude"] {
        let script = bin.join(tool);
        install_test_script_with_mode(
            &script,
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$AGENT_ARGS\"\n",
            0o700,
        );
    }
    let agent_args = fixture.root.path().join("agent-args");
    let title = "Fix API timeout; $(false)";
    let body = "Reproduce with two clients.\nKeep the connection alive.";
    for (index, (host, agent, as_url)) in [
        ("github.com", "codex", false),
        ("github.com", "claude", true),
        ("forge.example", "codex", true),
        ("forge.example", "claude", false),
    ]
    .into_iter()
    .enumerate()
    {
        let number = index + 34;
        let url = format!("https://{host}/team/project/issues/{number}");
        let remote = format!("git@{host}:team/project.git");
        if index == 0 {
            git(&fixture.repo, &["remote", "add", "origin", &remote]);
        } else {
            git(&fixture.repo, &["remote", "set-url", "origin", &remote]);
        }
        script_api(
            fixture.root.path(),
            &format!("repos/team/project/issues/{number}"),
            &serde_json::json!({"number": number, "title": title, "body": body,
                "state": "open", "labels": [], "pull_request": null}),
        );
        // The issue has no blocking issues.
        let dependencies = if host == "github.com" {
            format!(
                "repos/team/project/issues/{number}/dependencies/blocked_by?per_page=100&page=1"
            )
        } else {
            format!("repos/team/project/issues/{number}/dependencies?limit=50&page=1")
        };
        script_api(fixture.root.path(), &dependencies, &serde_json::json!([]));
        let input = if as_url {
            url.clone()
        } else {
            number.to_string()
        };
        let output = fixture
            .command()
            .args([
                "--json",
                "start",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                &input,
                "--here",
                "--agent",
                agent,
                "--",
                "--model",
                "test-model",
            ])
            .env("PATH", fixture.restricted_path(&bin))
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        let name = format!("{number}-fix-api-timeout");
        assert_eq!(workspace["name"], name);
        assert_eq!(workspace["branch"], name);
        assert_eq!(fixture.ok(&["status", &name])["issue"]["url"], url);
        let api = if host == "github.com" {
            "https://api.github.com"
        } else {
            "https://forge.example/api/v1"
        };
        let urls = fs::read_to_string(fixture.root.path().join("curl-urls")).unwrap();
        assert!(
            urls.contains(&format!("{api}/repos/team/project/issues/{number}\n")),
            "{urls}"
        );
        let invocation = strip_shoal_arguments(&fs::read_to_string(&agent_args).unwrap(), '\0');
        let prompt = invocation.split('\0').next().unwrap();
        assert!(prompt.contains(title));
        assert!(prompt.contains(&url));
        assert!(prompt.contains(body));
        assert!(invocation.contains("\0--model\0test-model\0"));
        assert_eq!(
            fixture.ok(&["status", &name])["executions"],
            serde_json::json!([])
        );
    }
    // An explicit branch name still loads the issue, with no agent required.
    let output = fixture
        .command()
        .args([
            "--json",
            "start",
            fixture.repo.to_str().unwrap(),
            "custom-issue-name",
            "--base",
            "HEAD",
            "--issue",
            "37",
        ])
        .env("PATH", fixture.restricted_path(&bin))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["name"],
        "custom-issue-name"
    );
}

#[test]
fn add_item_finds_the_repository_and_starts_the_default_agent() {
    let fixture = Fixture::with_config(Some("agent.default = 'claude'\n"));
    let other = fixture.root.path().join("other");
    git(
        &fixture.repo,
        &["clone", "-q", ".", other.to_str().unwrap()],
    );
    git(
        &other,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:team/other.git",
        ],
    );
    git(&other, &["config", "core.sshCommand", "false"]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    fixture.add_github_origin();
    let bin = fixture.root.path().join("issue-bin");
    // The daemon reads the issue through the scripted curl.
    install_api_curl(&fixture.root.path().join("bin"));
    for tool in ["codex", "claude"] {
        let script = bin.join(tool);
        install_test_script_with_mode(
            &script,
            "#!/bin/sh\nprintf '%s\\0' \"$(basename \"$0\")\" \"$@\" > \"$AGENT_ARGS\"\n",
            0o700,
        );
    }
    let agent_args = fixture.root.path().join("agent-args");
    let body = "Paste the URL and go.";
    for (number, agent, expected) in [
        (41, None, "claude"),
        (42, Some("codex"), "codex"),
        (43, None, "codex"),
        (45, None, "codex"),
        (46, None, "codex"),
        (47, None, "codex"),
    ] {
        if number == 43 {
            // The repository's checked-in default wins over the global one.
            fs::write(
                fixture.repo.join(".shoal.toml"),
                "agent.default = 'codex'\n",
            )
            .unwrap();
            git(&fixture.repo, &["add", ".shoal.toml"]);
            git(
                &fixture.repo,
                &[
                    "-c",
                    "user.name=Shoal Test",
                    "-c",
                    "user.email=shoal@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "default agent",
                ],
            );
        }
        let issue = format!("repos/team/project/issues/{number}");
        script_api(
            fixture.root.path(),
            &issue,
            &serde_json::json!({"state": "open", "number": number, "title": "Paste an issue",
                "body": body, "labels": []}),
        );
        // The issue has no blocking issues.
        script_api(
            fixture.root.path(),
            &format!("{issue}/dependencies/blocked_by?per_page=100&page=1"),
            &serde_json::json!([]),
        );
        let url = format!("https://github.com/team/project/issues/{number}");
        let number_input = number.to_string();
        let input = if number >= 45 { &number_input } else { &url };
        let mut args = vec!["--json", "start", input, "--base", "HEAD", "--here"];
        if number == 47 {
            args.extend(["--repo", fixture.repo.to_str().unwrap()]);
        }
        if let Some(agent) = agent {
            args.extend(["--agent", agent]);
        }
        args.extend(["--", "--model", "test-model"]);
        let cwd = match number {
            45 => fixture.repo.join("nested"),
            46 => PathBuf::from(
                fixture.ok(&["status", "41-paste-issue"])["workspace"]["path"]
                    .as_str()
                    .unwrap(),
            )
            .join("nested"),
            _ => other.clone(),
        };
        fs::create_dir_all(&cwd).unwrap();
        let output = fixture
            .command()
            .args(&args)
            .current_dir(cwd)
            .env("PATH", fixture.restricted_path(&bin))
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        let name = format!("{number}-paste-issue");
        assert_eq!(workspace["name"], name);
        assert_eq!(workspace["branch"], name);
        let urls = fs::read_to_string(fixture.root.path().join("curl-urls")).unwrap();
        assert!(
            urls.contains(&format!(
                "https://api.github.com/repos/team/project/issues/{number}\n"
            )),
            "{urls}"
        );
        let invocation = strip_shoal_arguments(&fs::read_to_string(&agent_args).unwrap(), '\0');
        let mut parts = invocation.split('\0');
        assert_eq!(parts.next(), Some(expected));
        let prompt = parts.next().unwrap();
        assert!(prompt.contains(&url) && prompt.contains(body), "{prompt}");
        assert!(invocation.contains("\0--model\0test-model\0"));
        assert_eq!(
            fixture.ok(&["status", &name])["executions"],
            serde_json::json!([])
        );
    }
    // `add` accepts numbers and URLs without applying either configured default agent.
    fs::remove_file(&agent_args).unwrap();
    for (number, repository, input) in [
        (44, None, "https://github.com/team/project/issues/44"),
        (68, Some(fixture.repo.to_str().unwrap()), "68"),
    ] {
        let issue = format!("repos/team/project/issues/{number}");
        script_api(
            fixture.root.path(),
            &issue,
            &serde_json::json!({"state": "open", "number": number, "title": "Create workspace",
                "body": body, "labels": []}),
        );
        script_api(
            fixture.root.path(),
            &format!("{issue}/dependencies/blocked_by?per_page=100&page=1"),
            &serde_json::json!([]),
        );
        let mut command = fixture.command();
        command.args(["--json", "start"]);
        if let Some(repository) = repository {
            command.arg(repository);
        }
        let output = command
            .args(["--issue", input, "--base", "HEAD"])
            .env("PATH", fixture.restricted_path(&bin))
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(workspace["name"], format!("{number}-create-workspace"));
        let inspection = fixture.ok(&["status", workspace["name"].as_str().unwrap()]);
        assert_eq!(inspection["workspace"]["state"], "ready");
        assert_eq!(inspection["executions"], serde_json::json!([]));
        assert!(!agent_args.exists());
    }

    // The default agent applies to the issue command, not ordinary additions.
    let output = fixture
        .command()
        .args([
            "--json",
            "start",
            fixture.repo.to_str().unwrap(),
            "plain",
            "--base",
            "HEAD",
        ])
        .env("PATH", fixture.restricted_path(&bin))
        .env("AGENT_ARGS", &agent_args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!agent_args.exists());
}

#[test]
fn issue_number_picks_a_repository_before_lookup_interactively() {
    let fixture = Fixture::with_config(Some("agent.default = 'claude'\n"));
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    install_fake_agents(&fixture, &["claude"]);
    install_test_script(bin.join("fzf"), "#!/bin/sh\nhead -n 1\n");
    // The scripted forge has no such issue, so the lookup fails after the pick.
    let (output, transcript) = fixture.interactive(&["start", "103", "--base", "HEAD"], "");
    assert!(!output.status.success(), "{output:?}\n{transcript}");
    assert!(
        transcript.contains("GET repos/team/project/issues/103"),
        "{transcript}"
    );
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
}

#[test]
fn issue_lookup_errors_never_create_a_workspace() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/team/project.git",
        ],
    );
    let bin = fixture.root.path().join("issue-bin");
    fs::create_dir(&bin).unwrap();
    // The command finds only Git and the agent.
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file() && path.metadata().unwrap().permissions().mode() & 0o111 != 0)
        .unwrap()
        .canonicalize()
        .unwrap();
    std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
    // Agents are checked before the issue lookup these cases exercise.
    install_test_script_with_mode(bin.join("codex"), "#!/bin/sh\n", 0o700);
    // The daemon reads the issue with the curl on its own PATH.
    let tool = fixture.root.path().join("bin/curl");
    for (input, script, diagnostic) in [
        (
            "4",
            Some("#!/bin/sh\necho login-required >&2\nexit 1\n"),
            "login-required",
        ),
        (
            "4",
            Some("#!/bin/sh\nprintf 'bad-json\\n0 200'\n"),
            "invalid JSON response",
        ),
        (
            "https://github.com/other/project/issues/4",
            None,
            "different repository",
        ),
    ] {
        if let Some(script) = script {
            install_test_script_with_mode(&tool, script, 0o700);
        }
        let output = fixture
            .command()
            .args([
                "start",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                input,
            ])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{output:?}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
    // A found issue still needs an agent before anything is created.
    install_test_script_with_mode(
        &tool,
        r#"#!/bin/sh
case "$*" in
  *blocked_by*) printf '[]\n0 200';;
  *) printf '{"state":"open","number":4,"title":"Four","body":"","labels":[]}\n0 200';;
esac
"#,
        0o700,
    );
    for (args, diagnostic) in [
        (
            vec!["start", "--issue", "https://github.com/team/other/issues/4"],
            "shoal repo add",
        ),
        (vec!["start", "--issue", "4"], "pass --repo"),
        (
            vec!["start", "https://github.com/team/project/issues/4"],
            "no agent selected",
        ),
        (vec!["start", "4", "--agent", "codex"], "pass --repo"),
        (
            vec![
                "start",
                fixture.repo.to_str().unwrap(),
                "--issue",
                "not-an-issue",
                "--agent",
                "codex",
            ],
            "a positive number or an issue URL",
        ),
        (
            vec![
                "start",
                "0",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "codex",
            ],
            "must be positive",
        ),
        (
            vec!["start", "4", "--repo", "unknown", "--agent", "codex"],
            "not registered",
        ),
        (
            vec![
                "start",
                "https://github.com/team/other/issues/4",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "codex",
            ],
            "different repository",
        ),
        (
            vec![
                "start",
                "https://github.com/team/other/issues/4",
                "--agent",
                "codex",
            ],
            "shoal repo add",
        ),
        (
            vec![
                "start",
                fixture.repo.to_str().unwrap(),
                "--issue",
                "https://github.com/team/project/pull/4",
                "--agent",
                "codex",
            ],
            "/issues/<number>",
        ),
    ] {
        let output = fixture
            .command()
            .args(&args)
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{output:?}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
}

#[test]
fn issues_reopen_their_branch_and_report_conflicts_before_picking_an_agent() {
    let fixture = Fixture::with_config(Some("[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n"));
    fixture.add_github_origin();
    fixture.script_github(&github_issue(34, "Fix API timeout", ""));
    let repo = fixture.repo.to_str().unwrap();
    let issue = ["start", "34", "--repo", repo, "--here", "--agent", "pi"];
    let branch = "34-fix-api-timeout";
    let branches = |fixture: &Fixture| -> Vec<Value> {
        fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["branch"].clone())
            .collect()
    };
    let created = fixture.ok(&[&issue[..], &["--base", "HEAD"]].concat());
    let reopened = fixture.ok(&issue);
    assert_eq!(reopened["id"], created["id"]);
    assert_eq!(branches(&fixture), [branch]);

    // A commit of its own keeps the branch when the workspace goes.
    let path = Path::new(created["path"].as_str().unwrap());
    commit_file(path, "work", "work\n");
    fixture.ok(&["rm", branch, "--yes"]);
    let elsewhere = fixture.root.path().join("elsewhere");
    git(
        &fixture.repo,
        &["worktree", "add", "-q", elsewhere.to_str().unwrap(), branch],
    );
    // Without --agent the picker would fail next; the conflict is reported first.
    let output = fixture.run(&issue[..4]);
    assert!(!output.status.success());
    assert!(branches(&fixture).is_empty());

    git(
        &fixture.repo,
        &["worktree", "remove", elsewhere.to_str().unwrap()],
    );
    fixture.ok(&issue);
    assert_eq!(branches(&fixture), [branch]);
}

#[test]
fn add_existing_branch_runs_setup_once_and_keeps_scoped_callers_to_their_tree() {
    let fixture = Fixture::new();
    for hook in ["setup", "post"] {
        let path = fixture.repo.join(hook);
        install_test_script(&path, format!("#!/bin/sh\necho {hook} >> runs\n"));
    }
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "setup.command = './setup'\nhooks.post_setup = './post'\n",
    )
    .unwrap();
    git(&fixture.repo, &["add", "."]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "hooks",
        ],
    );
    git(&fixture.repo, &["branch", "coworker"]);
    let args = [
        "start",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "coworker",
    ];
    let opened = fixture.ok(&args);
    let path = Path::new(opened["path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(path.join("runs")).unwrap(),
        "setup\npost\n"
    );
    let reopened = fixture.ok(&args);
    assert_eq!(opened["id"], reopened["id"]);
    assert_eq!(
        fs::read_to_string(path.join("runs")).unwrap(),
        "setup\npost\n"
    );
    // A workspace process opens a workspace below its own, but takes over
    // none outside it.
    fixture.add("elsewhere");
    let output = fixture.run(&[
        "run",
        "--workspace",
        "coworker",
        "--",
        support::shoal_binary(),
        "start",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "elsewhere",
    ]);
    assert!(!output.status.success());
    assert!(fixture.ok(&["status", "elsewhere"])["workspace"]["parent"].is_null());
    assert!(
        !fixture
            .run(&[
                "start",
                fixture.repo.to_str().unwrap(),
                "other",
                "--existing",
                "coworker",
            ])
            .status
            .success()
    );
    for flag in ["--base", "--issue"] {
        assert!(
            !fixture
                .run(&[
                    "start",
                    fixture.repo.to_str().unwrap(),
                    "--existing",
                    "coworker",
                    flag,
                    "other"
                ])
                .status
                .success()
        );
    }
}

#[test]
fn interactive_add_picks_existing_branch_and_reopens_workspace() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["branch", "coworker/topic"]);
    let bin = fixture.root.path().join("bin");
    let picker = bin.join("fzf");
    install_test_script(
        &picker,
        "#!/bin/sh\nawk -F '\\t' '$2 == \"Use an existing branch\" || $1 == \"refs/heads/coworker/topic\" {print}'\n",
    );
    let directive = fixture.root.path().join("destination");
    for _ in 0..2 {
        let (output, transcript) = fixture.interactive_command(
            fixture
                .command()
                .env("SHOAL_SHELL_DIRECTIVE", &directive)
                .args(["start", fixture.repo.to_str().unwrap(), "--mode", "worker"]),
            "",
        );
        assert!(output.status.success(), "{transcript}");
        let workspace = fixture.ok(&["status", "coworker-topic"]);
        assert_eq!(
            fs::read_to_string(&directive).unwrap().trim(),
            workspace["workspace"]["path"].as_str().unwrap()
        );
    }
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

#[test]
fn interactive_add_picks_an_open_issue_or_pull_request() {
    let fixture = Fixture::new();
    fixture.add("topic");
    fixture.add_github_origin();
    // An existing branch avoids refreshing the default branch from GitHub.
    git(&fixture.repo, &["branch", "34-fix-api-timeout"]);
    let mut responses =
        github_open_items(&[(34, "Fix API timeout"), (35, "Other")], &[(7, "Topic")]);
    responses.extend(github_issue(34, "Fix API timeout", ""));
    fixture.script_github(&responses);
    let picker = fixture.root.path().join("bin/fzf");
    let directive = fixture.root.path().join("destination");
    let add = |choice: &str, item: &str| {
        install_test_script(
            &picker,
            format!("#!/bin/sh\nawk -F '\\t' '$2 == \"{choice}\" || $2 ~ /^#{item} / {{print}}'\n"),
        );
        fixture.interactive_command(
            fixture
                .command()
                .env("SHOAL_SHELL_DIRECTIVE", &directive)
                .args(["start", fixture.repo.to_str().unwrap(), "--mode", "worker"]),
            "",
        )
    };
    let (output, transcript) = add("Use an issue", "34");
    assert!(output.status.success(), "{output:?} {transcript}");
    let issue = fixture.ok(&["status", "34-fix-api-timeout"]);
    assert_eq!(
        issue["issue"]["url"],
        "https://github.com/team/project/issues/34"
    );
    // The PR's head branch belongs to a workspace, which reopens.
    let (output, transcript) = add("Use a pull request", "7");
    assert!(output.status.success(), "{output:?} {transcript}");
    assert_eq!(
        fs::read_to_string(&directive).unwrap().trim(),
        fixture.ok(&["status", "topic"])["workspace"]["path"]
            .as_str()
            .unwrap()
    );
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 2);

    fixture.script_github(&github_open_items(&[], &[]));
    let (output, transcript) = add("Use a pull request", "7");
    assert!(!output.status.success(), "{transcript}");
}

pub(crate) fn issue_fixture(host: &str) -> Fixture {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    git(&fixture.repo, &["config", "protocol.allow", "never"]);
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://{host}/team/repo"),
        ],
    );
    install_api_curl(&fixture.root.path().join("bin"));
    if host == "github.com" {
        // The issue has no blocking issues.
        script_api(
            fixture.root.path(),
            "repos/team/repo/issues/316/dependencies/blocked_by?per_page=100&page=1",
            &serde_json::json!([]),
        );
    }
    write_issue_state(&fixture, "Open");
    fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "issue-work",
        "--issue",
        "316",
        "--base",
        "HEAD",
    ]);
    fixture
}

fn write_issue_state(fixture: &Fixture, state: &str) {
    let response = serde_json::json!({"number":316,"title":"Issue","body":"context",
        "state":state.to_lowercase(),"labels":[],"pull_request":null});
    script_api(fixture.root.path(), "repos/team/repo/issues/316", &response);
}

#[test]
fn already_closed_issues_are_rejected_before_creating_workspaces() {
    for host in ["github.com", "forge.example"] {
        let fixture = issue_fixture(host);
        write_issue_state(&fixture, "Closed");
        let before = fixture.ok(&["list"]);
        let output = fixture.run(&[
            "start",
            fixture.repo.to_str().unwrap(),
            "closed-work",
            "--issue",
            "316",
            "--base",
            "HEAD",
        ]);
        assert!(!output.status.success());
        assert_eq!(fixture.ok(&["list"]), before);
    }
}

#[test]
fn an_apps_creation_rejects_a_closed_issue_before_creating_a_workspace() {
    use shoal_client::{
        connection,
        protocol::{
            OperationMethod,
            operations::{StartedWorkspace, WorkspaceStart},
        },
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    {
        let fixture = issue_fixture("github.com");
        write_issue_state(&fixture, "Closed");
        let before = fixture.ok(&["list"]);
        let app = fixture.connector();
        let create = OperationMethod::StartWorkspace {
            repository: fixture.repo.to_str().unwrap().into(),
            start: WorkspaceStart::Name("closed-work".into()),
            base: Some("HEAD".into()),
            path: None,
            git_profile: None,
            parent: None,
            options: Default::default(),
            env: None,
            issue: Some("316".into()),
            pr: None,
            agent: None,
        };
        let refused = runtime.block_on(connection::request::<StartedWorkspace>(&app, create));
        assert!(refused.is_err(), "expected a refusal: {refused:?}");
        assert_eq!(fixture.ok(&["list"]), before);
    }
}

/// An app creates workspaces through `shoal_client` and follows their
/// preparation in the daemon's worker: setup's output as setup writes it,
/// the post-setup hook, then the ready workspace, or the typed failure of
/// setup or the hook.
#[test]
fn an_app_follows_the_setup_of_a_workspace_it_creates() {
    use shoal_client::{
        operations::{Followed, Following},
        protocol::{
            ErrorCode, OperationMethod,
            operations::{Ended, Outcome, Progress, StartedWorkspace, Step, WorkspaceStart},
        },
        state::WorkspaceState,
    };
    let fixture = Fixture::new();
    // Setup waits for `go`, which the app writes only once it has read the
    // first line, so that line arrived while setup ran.
    install_test_script(
        fixture.repo.join("setup.sh"),
        "#!/bin/sh\necho setting up\ncase \"$PWD\" in *setup-fails) exit 3 ;; esac\ni=0\nwhile [ ! -e go ]; do i=$((i+1)); [ $i -lt 600 ] || exit 4; sleep 0.1; done\necho set up\n",
    );
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        "#!/bin/sh\necho post-setup ran\ncase \"$PWD\" in *hook-fails) exit 5 ;; esac\n",
    );
    commit_resource_config(
        &fixture.repo,
        "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\n",
    );
    let app = fixture.connector();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let start = |name: &str| {
        let create = OperationMethod::StartWorkspace {
            repository: fixture.repo.to_str().unwrap().into(),
            start: WorkspaceStart::Name(name.into()),
            base: Some("HEAD".into()),
            path: None,
            git_profile: None,
            parent: None,
            options: Default::default(),
            env: None,
            issue: None,
            pr: None,
            agent: None,
        };
        runtime
            .block_on(shoal_client::connection::request::<StartedWorkspace>(
                &app, create,
            ))
            .unwrap()
    };
    let follow = |started: &StartedWorkspace| {
        let go = started.workspace.path.join("go");
        runtime.block_on(async {
            let mut following = Following::open(&app, started.operation.clone(), 0)
                .await
                .unwrap();
            let mut events = Vec::new();
            loop {
                match following.next().await.unwrap() {
                    Followed::Event(event) => {
                        if matches!(&event.progress, Progress::Output { line, .. } if line == "setting up")
                        {
                            fs::write(&go, "").unwrap();
                        }
                        events.push(event.progress);
                    }
                    Followed::Ended(ended) => return (events, ended),
                }
            }
        })
    };
    let lines = |events: &[Progress]| -> Vec<String> {
        events
            .iter()
            .filter_map(|progress| match progress {
                Progress::Output { line, .. } => Some(line.clone()),
                Progress::Step { .. } => None,
            })
            .collect()
    };

    let (events, ended) = follow(&start("api"));
    let Ended::Succeeded(outcome) = ended else {
        panic!("expected the prepared workspace: {ended:?} after {events:?}");
    };
    let Outcome::Prepared(prepared) = *outcome else {
        panic!("expected the prepared workspace: {outcome:?}");
    };
    assert_eq!(prepared.workspace.state, WorkspaceState::Ready);
    let steps: Vec<Step> = events
        .iter()
        .filter_map(|progress| match progress {
            Progress::Step { step } => Some(*step),
            Progress::Output { .. } => None,
        })
        .collect();
    assert_eq!(
        steps,
        [Step::SetupStarted, Step::Ready, Step::PostSetupStarted]
    );
    let lines = lines(&events);
    for line in ["setting up", "set up", "post-setup ran"] {
        assert!(lines.iter().any(|shown| shown == line), "{line}: {lines:?}");
    }

    for (name, code) in [
        ("setup-fails", ErrorCode::SetupFailed),
        ("hook-fails", ErrorCode::HookFailed),
    ] {
        let (events, ended) = follow(&start(name));
        let Ended::Failed(failure) = ended else {
            panic!("{name}: expected a failure: {ended:?} after {events:?}");
        };
        assert_eq!(failure.code(), code, "{name}: {failure:?}");
    }
}

#[test]
fn split_without_a_branch_creates_a_workspace_at_a_commit() {
    let fixture = Fixture::new();
    fixture.add("parent");
    let main = git(&fixture.repo, &["rev-parse", "HEAD"]);
    let short = &main.trim()[..7];
    // Outside a workspace it is top-level, in the current checkout's repository.
    let top = fixture
        .command()
        .current_dir(&fixture.repo)
        .args(["--json", "split", "--no-branch"])
        .output()
        .unwrap();
    assert!(top.status.success(), "{top:?}");
    let top: Value = serde_json::from_slice(&top.stdout).unwrap();
    assert_eq!(top["name"], format!("main-{short}"));
    assert_eq!(top["branch"], Value::Null);
    assert_eq!(top["at"]["commit"], main.trim());
    assert_eq!(top["at"]["made_for"], serde_json::json!({"ref": "main"}));
    assert_eq!(top["parent"], Value::Null);
    let path = top["path"].as_str().unwrap();
    assert_eq!(
        git(Path::new(path), &["branch", "--show-current"]).trim(),
        ""
    );
    let child = fixture.ok(&["split", "--no-branch", "--workspace", "parent"]);
    assert_eq!(child["parent"]["name"], "parent");
    assert_eq!(child["name"], format!("main-{short}-2"));
    for args in [
        &["split", "--no-branch", "child"][..],
        &["split", "--no-branch", "--issue", "4"],
    ] {
        assert!(!fixture.run(args).status.success(), "{args:?}");
    }
}

#[test]
fn a_workspace_created_without_setup_skips_its_hooks_until_setup_runs() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    for (name, hook) in [
        ("setup.sh", "setup"),
        ("post-setup.sh", "post-setup"),
        ("pre-remove.sh", "pre-remove"),
    ] {
        install_test_script(
            fixture.repo.join(name),
            format!("#!/bin/sh\necho \"$SHOAL_WORKSPACE\" >> \"$HOME/{hook}-ran\"\n"),
        );
    }
    commit_resource_config(
        &fixture.repo,
        "setup.command = 'setup.sh'\nhooks.post_setup = 'post-setup.sh'\nhooks.pre_remove = 'pre-remove.sh'\n",
    );
    let ran = |hook: &str| fs::read_to_string(home.join(format!("{hook}-ran"))).unwrap_or_default();
    let repo = fixture.repo.to_str().unwrap();
    let skipped = fixture.ok(&["start", repo, "skipped", "--skip-setup"]);
    assert_eq!(skipped["state"], "ready");
    assert_eq!(skipped["setup_skipped"], true);
    let at_commit = fixture.ok(&[
        "split",
        "--no-branch",
        "--workspace",
        "skipped",
        "--skip-setup",
    ]);
    assert_eq!(at_commit["setup_skipped"], true);
    assert_eq!(
        (ran("setup"), ran("post-setup")),
        (String::new(), String::new())
    );
    fixture.ok(&["rm", "skipped", "--yes"]);
    assert_eq!(ran("pre-remove"), "");

    // Setup run later clears the mark, so the hooks around it run again.
    fixture.ok(&["start", repo, "later", "--skip-setup"]);
    let set_up = fixture.ok(&["setup", "later"]);
    assert_eq!(set_up["setup_skipped"], false);
    assert_eq!(
        (ran("setup"), ran("post-setup")),
        ("later\n".into(), "later\n".into())
    );
    fixture.ok(&["rm", "later", "--yes"]);
    assert_eq!(ran("pre-remove"), "later\n");
    // With only a post-setup hook, setup run later still clears the mark and
    // runs the hook.
    commit_resource_config(&fixture.repo, "hooks.post_setup = 'post-setup.sh'\n");
    fixture.ok(&["start", repo, "hooked", "--skip-setup"]);
    let set_up = fixture.ok(&["setup", "hooked"]);
    assert_eq!(set_up["setup_skipped"], false);
    assert!(
        ran("post-setup").ends_with("hooked\n"),
        "{}",
        ran("post-setup")
    );
}

#[test]
fn branch_only_commands_refuse_a_workspace_at_a_commit() {
    let fixture = Fixture::new();
    fixture.add("parent");
    let created = fixture.ok(&["split", "--no-branch", "--workspace", "parent"]);
    let name = created["name"].as_str().unwrap().to_owned();
    let path = PathBuf::from(created["path"].as_str().unwrap());
    let run = |args: &[&str]| {
        fixture
            .command()
            .current_dir(&path)
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        &["pr", "open"][..],
        &["ready"],
        &["done"],
        &["merge"],
        &["branch", "rename", "renamed"],
    ] {
        let output = run(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains("is at a commit"), "{args:?}: {stderr}");
    }
    // What needs no branch works there.
    let output = run(&["run", "--", "git", "rev-parse", "HEAD"]);
    assert!(output.status.success(), "{output:?}");
    assert!(fixture.run(&["diff", &name]).status.success());
}
