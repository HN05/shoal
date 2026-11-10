//! Pickers, navigation, shell completion, the inbox, and the status and lists
//! of spaces.
use super::approvals::pending_approval;
use super::forge::{forgejo_pull, install_api_curl, script_api};
use super::resources::RESOURCE_CONFIG;
use super::zmx::{agent_in_session, zmx_calls};
use super::*;

#[test]
fn setup_completion_exempts_only_the_callers_own_execution() {
    let fixture = Fixture::new();
    fixture.add("first");
    let shoal = support::shoal_binary();
    // Completion for setup, run inside `first` through `depth` nested execs.
    let complete = |depth: usize| {
        let mut words = vec![];
        for _ in 0..depth {
            words.extend([shoal, "run", "--workspace", "first", "--"]);
        }
        words.extend(["env", "SHOAL_COMPLETE=zsh", "_CLAP_COMPLETE_INDEX=3"]);
        words.extend([shoal, "--", "shoal", "work", "setup", ""]);
        let output = fixture.command().args(&words[1..]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let own = complete(1);
    assert!(own.lines().any(|line| line == "first"), "{own}");
    let nested = complete(2);
    assert!(
        nested.lines().any(|line| line == "first:running"),
        "{nested}"
    );
}

#[test]
fn live_completion_uses_targets_state_override_workspace_context_and_scope() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    let first = fixture.add("first");
    fixture.add("second");
    fixture.ok(&["acquire", "port", "web", "--workspace", "first"]);
    fixture.ok(&[
        "acquire",
        "resource",
        "devices",
        "--workspace",
        "first",
        "--name",
        "tests",
    ]);
    let complete = |args: &[&str], cwd: &Path| {
        let state = fixture.root.path().join("state");
        let mut words = vec!["shoal", "--state-dir", state.to_str().unwrap()];
        words.extend_from_slice(args);
        let output = fixture
            .command()
            .arg("--")
            .args(&words)
            .env("SHOAL_COMPLETE", "bash")
            .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
            .env("SHOAL_STATE_DIR", fixture.root.path().join("wrong-state"))
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for args in [
        vec!["repo", "rm", "pr"],
        vec!["start", "103", "--repo", "pr"],
    ] {
        assert!(
            complete(&args, fixture.root.path()).contains(&"project".into()),
            "{args:?}"
        );
    }
    assert!(complete(&["work", "status", "fi"], fixture.root.path()).contains(&"first".into()));
    // A command taking several workspaces offers each one once.
    let choices = complete(&["rm", "first", ""], fixture.root.path());
    assert!(choices.contains(&"second".into()), "{choices:?}");
    assert!(!choices.contains(&"first".into()), "{choices:?}");
    for words in [
        vec!["run", "--workspace", "fi"],
        vec!["link", "pr", "42", "assigned", "--space", "fi"],
    ] {
        assert!(complete(&words, fixture.root.path()).contains(&"first".into()));
    }
    let choices = complete(&["repo", "rm", ""], fixture.root.path());
    let target = choices.iter().position(|v| v == "project").unwrap();
    assert!(
        choices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.starts_with('-'))
            .all(|(i, _)| i > target)
    );
    // `--all` comes once, before a scope's workspaces, until it is typed.
    let choices = complete(&["resume", ""], fixture.root.path());
    assert_eq!(choices[0], "--all", "{choices:?}");
    assert!(choices.contains(&"first".into()), "{choices:?}");
    assert_eq!(choices.iter().filter(|v| *v == "--all").count(), 1);
    // Resume describes each workspace by what it would restore.
    let resumable = || {
        let output = fixture
            .command()
            .args(["--", "shoal", "resume", ""])
            .env("SHOAL_COMPLETE", "zsh")
            .env("_CLAP_COMPLETE_INDEX", "2")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    fixture.save_stopped_command(&first);
    let choices = resumable();
    assert!(choices.contains("first:stopped: 1 command"), "{choices}");
    // Other commands name why they would refuse a workspace.
    let described = |args: &[&str]| {
        let mut words = vec!["shoal"];
        words.extend_from_slice(args);
        let output = fixture
            .command()
            .arg("--")
            .args(&words)
            .env("SHOAL_COMPLETE", "zsh")
            .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    // Workspaces without what the command acts on are left out.
    for args in [
        vec!["stop", ""],
        vec!["done", "--undo", ""],
        vec!["ready", "--undo", "--space", ""],
        vec!["release", "hold", "--workspace", ""],
        vec!["branch", "base", "clear", "--workspace", ""],
    ] {
        let choices = described(&args);
        assert!(
            !choices.iter().any(|choice| choice.starts_with("second")),
            "{args:?} {choices:?}"
        );
    }
    fixture.ok(&[
        "acquire",
        "hold",
        "--workspace",
        "second",
        "--name",
        "review",
    ]);
    assert!(described(&["release", "hold", "--workspace", ""]).contains(&"second".into()));
    // A workspace that is not ready shows its state.
    let failed = fixture
        .command()
        .args([
            "start",
            fixture.repo.to_str().unwrap(),
            "broken",
            "--base",
            "not-a-ref",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let choices = described(&["run", "--workspace", ""]);
    assert!(choices.contains(&"broken:failed".into()), "{choices:?}");
    assert!(choices.contains(&"second".into()), "{choices:?}");
    fixture.ok(&[
        "release",
        "hold",
        "--workspace",
        "second",
        "--name",
        "review",
    ]);
    assert!(described(&["run", "--workspace", ""]).contains(&"second".into()));
    assert!(!complete(&["stop", "--all", ""], fixture.root.path()).contains(&"--all".into()));
    let cwd = Path::new(first["path"].as_str().unwrap());
    fs::write(cwd.join(".shoal.toml"), "[commands]\nreview = ['tuicr']\n").unwrap();
    // Complete the command name with an unknown workspace already on the line.
    let names = fixture
        .command()
        .env("SHOAL_STATE_DIR", fixture.root.path().join("state"))
        .args(["--", "shoal", "run", "rev", "--workspace", "unknown"])
        .env("SHOAL_COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "2")
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(names.status.success(), "{names:?}");
    assert!(
        String::from_utf8_lossy(&names.stdout)
            .lines()
            .any(|line| line == "review")
    );
    assert!(complete(&["rev"], cwd).contains(&"review".into()));
    assert!(complete(&["review", "run", "fi"], cwd).contains(&"first".into()));
    assert!(complete(&["acquire", "port", "w"], cwd).contains(&"web".into()));
    assert!(complete(&["release", "port", "w"], cwd).contains(&"web".into()));
    assert!(complete(&["acquire", "resource", "d"], cwd).contains(&"devices".into()));
    assert!(
        complete(
            &[
                "acquire",
                "resource",
                "devices",
                "--workspace",
                "first",
                "--member",
                "b"
            ],
            fixture.root.path()
        )
        .contains(&"beta".into())
    );
    assert!(
        complete(
            &[
                "release",
                "resource",
                "devices",
                "--workspace",
                "first",
                "--name",
                "t"
            ],
            fixture.root.path()
        )
        .contains(&"tests".into())
    );
    assert!(
        !complete(
            &[
                "acquire",
                "resource",
                "devices",
                "--workspace",
                "unknown",
                "--member",
                "b",
            ],
            cwd,
        )
        .contains(&"beta".into())
    );
    fixture.ok(&["rm", "second"]);
    assert!(!complete(&["rm", ""], cwd).contains(&"second".into()));
    fixture.add("second");
    let scoped = fixture.run(&[
        "run",
        "--workspace",
        "first",
        "--",
        "env",
        "SHOAL_COMPLETE=bash",
        "_CLAP_COMPLETE_INDEX=2",
        support::shoal_binary(),
        "--",
        "shoal",
        "rm",
        "",
    ]);
    assert!(
        scoped.status.success(),
        "{}",
        String::from_utf8_lossy(&scoped.stderr)
    );
    let text = String::from_utf8(scoped.stdout).unwrap();
    assert!(text.lines().any(|line| line == "first"), "{text}");
    assert!(!text.lines().any(|line| line == "second"), "{text}");
    assert!(!fixture.root.path().join("wrong-state").exists());
}

#[test]
fn workspace_context_adapters_preserve_directory_scope_and_picker_policy() {
    let fixture = Fixture::new();
    let own = fixture.add("context-own");
    let other = fixture.add("context-other");
    let own_path = Path::new(own["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    for (path, name) in [(own_path, "own-only"), (other_path, "other-only")] {
        fs::write(
            path.join(".shoal.toml"),
            format!("[commands]\n{name} = ['true']\n"),
        )
        .unwrap();
    }
    let nested = own_path.join("nested/deep");
    fs::create_dir_all(&nested).unwrap();
    let alias = fixture.root.path().join("alias");
    std::os::unix::fs::symlink(&nested, &alias).unwrap();

    let command = |cwd: &Path, scoped: bool, completion: bool| {
        if scoped {
            let mut command = fixture.command();
            command
                .args([
                    "run",
                    "--workspace",
                    "context-own",
                    "--",
                    "sh",
                    "-c",
                    "cd \"$1\"; shift; exec \"$@\"",
                    "context-test",
                ])
                .arg(cwd)
                .arg("env");
            // Completion variables belong to the inner process, after scope delivery.
            if completion {
                command.args(["SHOAL_COMPLETE=bash", "_CLAP_COMPLETE_INDEX=2"]);
            }
            command.arg(support::shoal_binary());
            command
        } else {
            let mut command = fixture.command();
            command.current_dir(cwd);
            if completion {
                command
                    .env("SHOAL_COMPLETE", "bash")
                    .env("_CLAP_COMPLETE_INDEX", "2")
                    .env("SHOAL_STATE_DIR", fixture.root.path().join("state"));
            }
            command
        }
    };
    for (cwd, scoped) in [
        (nested.as_path(), false),
        (alias.as_path(), false),
        (fixture.root.path(), true),
        (other_path, true),
    ] {
        for args in [vec!["--json", "run"], vec!["--json", "config", "show"]] {
            let output = command(cwd, scoped, false).args(&args).output().unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("own-only"), "{text}");
            assert!(!text.contains("other-only"), "{text}");
        }
        let output = command(cwd, scoped, false)
            .args(["--json", "work", "status"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["workspace"]["id"], own["id"]);

        let output = command(cwd, scoped, true)
            .args(["--", "shoal", "run", "own"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "own-only")
        );
    }
    for args in [
        vec!["work", "status", "context-other"],
        vec!["stop"],
        vec!["config", "show", "context-other"],
    ] {
        let output = command(other_path, true, false)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
    }
    let output = command(&nested, false, false).arg("cd").output().unwrap();
    assert!(!output.status.success());
    let output = command(fixture.root.path(), false, false)
        .args(["--json", "run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("own-only"));
    let output = command(fixture.root.path(), false, false)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn status_summarizes_current_workspace_work_and_supports_json() {
    let config = format!("{RESOURCE_CONFIG}\n[cleanup.pr]\nenabled=false\n");
    let fixture = Fixture::with_config(Some(&config));
    let workspace = fixture.add("summary");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "changed\nagain\n").unwrap();
    fs::write(path.join("added"), "new\n").unwrap();
    git(path, &["add", "added"]);
    fixture.ok(&["acquire", "port", "web", "--workspace", "summary"]);
    fixture.ok(&[
        "acquire",
        "resource",
        "devices",
        "--workspace",
        "summary",
        "--name",
        "tests",
    ]);
    fixture.ok(&["acquire", "resource", "signing", "--workspace", "summary"]);
    fixture.add("waiter");
    assert_eq!(
        fixture
            .run(&["acquire", "resource", "signing", "--workspace", "waiter"])
            .status
            .code(),
        Some(2)
    );
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
        rusqlite::params![
            workspace["id"].as_str().unwrap(),
            serde_json::json!({
                "url": "https://forge.example/team/repo/pulls/7",
                "head": null,
                "error": null
            })
            .to_string()
        ],
    )
    .unwrap();

    let started = fixture.root.path().join("status-started");
    let finish = fixture.root.path().join("status-finish");
    let mut execution = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "summary",
            "--",
            "sh",
            "-c",
            "touch \"$1\"; while test ! -f \"$2\"; do sleep 0.02; done",
            "status-test",
            started.to_str().unwrap(),
            finish.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    wait_until("execution to start", || started.exists());

    let output = fixture
        .command()
        .args(["--json", "work", "status"])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(status.get("inspection").is_none());
    assert_eq!(status["workspace"]["name"], "summary");
    assert_eq!(status["workspace"]["branch"], "summary");
    assert_eq!(status["workspace"]["state"], "ready");
    assert_eq!(status["setup_finished"], true);
    assert_eq!(
        status["diff"],
        serde_json::json!({"files_changed": 2, "insertions": 3, "deletions": 1})
    );
    assert_eq!(status["executions"].as_array().unwrap().len(), 1);
    assert_eq!(status["ports"].as_array().unwrap().len(), 1);
    assert_eq!(status["resources"].as_array().unwrap().len(), 2);
    assert_eq!(status["simulators"], serde_json::json!([]));
    assert_eq!(
        status["pr_cleanup"]["url"],
        "https://forge.example/team/repo/pulls/7"
    );
    // The busy signing permit notified about the waiter, not this workspace.
    assert_eq!(status["unread_notifications"], 0);
    assert_eq!(
        fixture.ok(&["work", "status", "waiter"])["unread_notifications"],
        1
    );
    assert_eq!(status["prs"][0]["state"], Value::Null);
    assert!(status["prs"][0]["errors"][0].is_string(), "{status}");

    git(
        &fixture.repo,
        &["remote", "add", "origin", "https://forge.example/team/repo"],
    );
    let home = fixture.root.path();
    install_api_curl(&home.join("bin"));
    let sha = "7".repeat(40);
    let mut pull = forgejo_pull(7, "open", "summary", &sha);
    pull["mergeable"] = false.into();
    script_api(home, "repos/team/repo/pulls/7", &pull);
    script_api(
        home,
        &format!("repos/team/repo/commits/{sha}/status?limit=50"),
        &serde_json::json!({"total_count": 2, "statuses": [
            {"context": "ci / rust", "status": "success"}, {"context": "review", "status": "pending"}
        ]}),
    );
    let status = fixture.ok(&["work", "status", "summary"]);
    assert_eq!(
        status["prs"][0]["url"],
        "https://forge.example/team/repo/pulls/7"
    );
    assert_eq!(status["prs"][0]["state"], "open");
    assert_eq!(status["prs"][0]["merge_conflicts"], true);
    assert_eq!(
        status["prs"][0]["checks"],
        serde_json::json!([
            {"name": "ci / rust", "result": "success"},
            {"name": "review", "result": "pending"}
        ])
    );
    assert_eq!(status["prs"][0]["review"], Value::Null);
    assert_eq!(status["cleanup_reason"]["reason"], "pr_watch");
    assert!(status["prs"][0]["errors"][0].is_string(), "{status}");

    let text = fixture.run(&["work", "status", "summary"]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    for expected in [
        "2 files, +3 -1",
        "open, merge conflicts",
        "ci / rust: success\n    review: pending\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text:?}");
    }
    assert!(!fixture.run(&["work", "status"]).status.success());

    for target in [
        vec!["pr", "7"],
        vec!["https://forge.example/team/repo/pulls/7/"],
        vec!["resource", "devices"],
        vec!["resource", "signing"],
    ] {
        let found = fixture.ok(&[&["work", "status"], target.as_slice()].concat());
        let found = found.as_array().unwrap();
        assert_eq!(found.len(), 1, "{target:?}");
        assert_eq!(found[0]["workspace"]["name"], "summary");
        assert_eq!(found[0]["prs"][0]["state"], "open");
    }
    for target in [
        vec!["pr", "8"],
        vec!["issue", "7"],
        vec!["https://forge.example/team/other/pulls/7"],
        vec!["resource", "missing"],
        vec!["pr"],
        vec!["ci", "2765"],
        vec!["summary", "7"],
    ] {
        let output = fixture.run(&[&["work", "status"], target.as_slice()].concat());
        assert!(!output.status.success(), "{target:?}");
    }
    let scoped = |workspace: &str| {
        fixture
            .command()
            .args([
                "run",
                "--workspace",
                workspace,
                "--",
                support::shoal_binary(),
            ])
            .args(["--json", "work", "status", "resource", "devices"])
            .output()
            .unwrap()
    };
    let own = scoped("summary");
    assert!(own.status.success());
    let own: Value = serde_json::from_slice(&own.stdout).unwrap();
    assert_eq!(own[0]["workspace"]["name"], "summary");
    assert!(!scoped("waiter").status.success());

    fs::write(finish, "done").unwrap();
    assert!(execution.wait().unwrap().success());
}

#[test]
fn status_keeps_shared_state_when_the_worktree_is_missing() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    let workspace = fixture.add("missing-status");
    fixture.ok(&["acquire", "port", "web", "--workspace", "missing-status"]);
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();

    let status = fixture.ok(&["work", "status", "missing-status"]);
    assert_eq!(status["workspace"]["name"], "missing-status");
    assert_eq!(status["ports"].as_array().unwrap().len(), 1);
    assert!(status["diff"].is_null());
    assert!(!status["diff_error"].as_str().unwrap().is_empty());
}

#[test]
fn workspace_targets_precede_directory_and_picker_fallback() {
    let fixture = Fixture::new();
    let own = fixture.add("selection-own");
    let other = fixture.add("selection-other");
    let nested = Path::new(own["path"].as_str().unwrap()).join("nested");
    fs::create_dir(&nested).unwrap();
    let bin = fixture.root.path().join("bin");
    let picked = fixture.root.path().join("picked");
    let fzf = bin.join("fzf");
    install_test_script(
        &fzf,
        "#!/bin/sh\ntouch \"$HOME/picked\"\nawk -F '\\t' -v id=\"$PICK_ID\" '$1 == id {print}'\n",
    );

    for action in [&["work", "status"][..], &["stop"]] {
        let pick = other["id"].as_str().unwrap();
        for (cwd, explicit, json, expected, picker) in [
            (nested.as_path(), None, false, &own, false),
            (nested.as_path(), None, true, &own, false),
            (
                nested.as_path(),
                Some("selection-other"),
                false,
                &other,
                false,
            ),
            (nested.as_path(), other["id"].as_str(), true, &other, false),
            (fixture.root.path(), None, false, &other, true),
        ] {
            let (_master, slave) = pty::open();
            let mut command = fixture.command();
            command
                .current_dir(cwd)
                .env("PICK_ID", pick)
                .stdin(slave.try_clone().unwrap())
                .stderr(slave);
            if json {
                command.arg("--json");
            }
            command.args(action).args(explicit);
            let output = command.output().unwrap();
            if picker && action == ["stop"] {
                // Stop leaves the idle workspaces out of its picker, which
                // has nothing to offer.
                assert!(!output.status.success(), "{output:?}");
                assert!(!picked.exists());
                continue;
            }
            assert!(output.status.success(), "{action:?}: {output:?}");
            assert_eq!(picked.exists(), picker);
            if picker {
                fs::remove_file(&picked).unwrap();
            }
            if action == ["work", "status"] && json {
                let result: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["workspace"]["id"], expected["id"]);
            } else if action == ["work", "status"] {
                let text = String::from_utf8_lossy(&output.stdout);
                let name = expected["name"].as_str().unwrap();
                assert!(text.starts_with(&format!("{name} ")), "{text}");
            }
        }
        for (cwd, args) in [
            (nested.as_path(), [action, &["missing"]].concat()),
            (fixture.root.path(), [action, &["missing"]].concat()),
            (fixture.root.path(), [&["--json"], action].concat()),
        ] {
            let (_master, slave) = pty::open();
            let output = fixture
                .command()
                .current_dir(cwd)
                .env("PICK_ID", other["id"].as_str().unwrap())
                .args(args)
                .stdin(slave.try_clone().unwrap())
                .stderr(slave)
                .output()
                .unwrap();
            assert!(!output.status.success(), "{output:?}");
            assert!(!picked.exists());
        }
    }
}

#[test]
fn workspace_commands_select_from_a_deleted_current_directory() {
    use std::{ffi::CString, os::unix::process::CommandExt};

    let fixture = Fixture::new();
    fixture.add("available");
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let picked = fixture.root.path().join("picked");
    let fzf = bin.join("fzf");
    // Bash reports getcwd failures at startup if the picker inherits a deleted cwd.
    install_test_script(&fzf, "#!/bin/bash\npwd -P > \"$HOME/picked\"\ncat\n");
    fs::write(
        fixture.root.path().join("zmx-sessions"),
        "  name=available\tclients=0\n",
    )
    .unwrap();
    let mut agent = Some(agent_in_session(&fixture, "available", "available"));

    for args in [vec!["work", "status"], vec!["attach"], vec!["rm", "--yes"]] {
        if args[0] == "rm" {
            drop(agent.take());
        }
        let deleted = fixture.root.path().join("deleted");
        fs::create_dir(&deleted).unwrap();
        let path = CString::new(deleted.as_os_str().as_encoded_bytes()).unwrap();
        let mut command = fixture.command();
        command.current_dir(&deleted).args(&args);
        // Delete only the child's cwd, after chdir and before exec; never change
        // the test process's directory or the fixture's managed worktree.
        unsafe {
            command.pre_exec(move || {
                if libc::rmdir(path.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let (mut master, slave) = pty::open();
        command
            .stdin(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap());
        let output = command.output().unwrap();
        drop(command);
        // macOS discards unread output once the last slave closes.
        use std::io::Read;
        let mut transcript = String::new();
        let _ = master.read_to_string(&mut transcript);
        drop(slave);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        if args[0] == "attach" {
            let attached = zmx_calls(&fixture)
                .into_iter()
                .rfind(|call| call[0] == "attach");
            assert_eq!(attached.unwrap(), ["attach", "available"]);
        } else {
            assert!(transcript.is_empty(), "{transcript}");
        }
        if args[0] == "work" {
            assert!(stdout.starts_with("available "), "{stdout}");
        } else if args[0] == "attach" {
            assert!(stdout.is_empty(), "{stdout}");
        } else {
            assert!(fixture.ok(&["work"]).as_array().unwrap().is_empty());
        }
        assert_eq!(fs::read_to_string(&picked).unwrap().trim(), "/");
        fs::remove_file(&picked).unwrap();
    }
}

#[test]
fn noninteractive_missing_targets_do_not_open_pickers() {
    let fixture = Fixture::with_config(Some("[commands]\ncheck = ['true']\n"));
    fixture.add("available");
    let picked = fixture.root.path().join("picked");
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        "#!/bin/sh\ntouch \"$HOME/picked\"\nhead -n 1\n",
    );
    for args in [
        &["start"][..],
        &["run", "--", "true"],
        &["rm"],
        &["work", "status"],
        &["stop"],
        &["diff"],
        &["view", "pr", "12"],
        &["done"],
        &["check"],
    ] {
        for json in [false, true] {
            let mut command = fixture.command();
            if json {
                command.arg("--json");
            }
            let output = command.args(args).output().unwrap();
            assert!(!output.status.success(), "{args:?}: {output:?}");
            assert!(!picked.exists(), "{args:?}");
        }
    }
}

#[test]
fn shell_function_navigates_after_add_and_away_after_rm() {
    let fixture = Fixture::new();
    let integration = fixture.root.path().join("integration.sh");
    fs::write(&integration, fixture.run(&["shell", "init"]).stdout).unwrap();
    let script = r#"
set -e
. "$INTEGRATION"
cd "$REPO"
shoal start "$REPO" navigate
test "${PWD##*/}" = navigate
shoal_repo_dir="$(dirname "$PWD")"
shoal start "$REPO" other
test "${PWD##*/}" = other
shoal cd navigate
shoal cd -
test "${PWD##*/}" = other
shoal cd -
test "${PWD##*/}" = navigate
cd "$REPO"
shoal cd navigate
test "${PWD##*/}" = navigate
if shoal run --workspace navigate -- sh -c 'exit 7'; then
  exit 1
else
  test "$?" -eq 7
fi
printf 'keep me' > untracked
if shoal rm; then
  exit 1
fi
test "${PWD##*/}" = navigate
test -f untracked
rm untracked
mkdir nested
cd nested
shoal rm
test "$PWD" = "$shoal_repo_dir"
shoal cd -
test "${PWD##*/}" = other
shoal rm
test "$PWD" = "$shoal_repo_dir"
if shoal cd -; then
  exit 1
fi
test "$PWD" = "$shoal_repo_dir"
shoal start "$REPO" acknowledged
test "${PWD##*/}" = acknowledged
printf 'retained' > untracked
shoal done
test "${PWD##*/}" = acknowledged
shoal done --undo
rm untracked
shoal done
until ! shoal work status acknowledged >/dev/null 2>&1; do sleep 0.1; done
_shoal_recover_directory
test "$PWD" = "$shoal_repo_dir"
shoal start "$REPO" held
shoal acquire hold --workspace held --name review
shoal done --cleanup
test "${PWD##*/}" = held
shoal release hold --workspace held --name review
until ! shoal work status held >/dev/null 2>&1; do sleep 0.1; done
_shoal_recover_directory
test "$PWD" = "$shoal_repo_dir"
shoal start "$REPO" completed
printf 'keep me' > untracked
shoal done --cleanup
test "${PWD##*/}" = completed
shoal done --undo
rm untracked
shoal done --cleanup
until ! shoal work status completed >/dev/null 2>&1; do sleep 0.1; done
_shoal_recover_directory
test "$PWD" = "$shoal_repo_dir"
printf 'navigation-ok\n'
"#;
    for shell in ["bash", "zsh"] {
        let output = support::isolated(fixture.root.path(), shell)
            .arg("-c")
            .arg(script)
            .env("INTEGRATION", &integration)
            .env("REPO", &fixture.repo)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(support::shoal_binary())
                        .parent()
                        .unwrap()
                        .display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).ends_with("navigation-ok\n"));
    }
}

#[test]
fn interactive_navigation_reports_missing_shell_integration() {
    let fixture = Fixture::new();
    let workspace = fixture.add("existing");

    let (cd, cd_stderr) = fixture.interactive(&["cd", "existing"], "");
    assert!(cd.status.success());
    assert_eq!(
        String::from_utf8(cd.stdout).unwrap(),
        format!("{}\n", workspace["path"].as_str().unwrap())
    );
    assert!(cd_stderr.contains("shell integration"), "{cd_stderr}");

    let piped = fixture.run(&["cd", "existing"]);
    assert!(piped.status.success());
    assert!(piped.stderr.is_empty());
    assert_eq!(
        String::from_utf8(piped.stdout).unwrap(),
        format!("{}\n", workspace["path"].as_str().unwrap())
    );

    let json = fixture.ok(&["cd", "existing"]);
    assert_eq!(json["path"], workspace["path"]);
}

#[test]
fn cd_previous_returns_to_the_workspace_left_and_json_never_navigates() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let second = fixture.add("second");
    let path = |workspace: &Value| {
        fs::canonicalize(workspace["path"].as_str().unwrap())
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    };
    let directive = fixture.root.path().join("cd-directive");
    fs::write(&directive, "").unwrap();
    let cd = |from: &Path, args: &[&str]| {
        fixture
            .command()
            .current_dir(from)
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args(args)
            .output()
            .unwrap()
    };
    let none = cd(&fixture.repo, &["cd", "-"]);
    assert!(!none.status.success());
    let first_path = path(&first);
    let second_path = path(&second);
    assert!(
        cd(Path::new(&first_path), &["--json", "cd", "second"])
            .status
            .success()
    );
    for (from, to) in [(&second_path, &first_path), (&first_path, &second_path)] {
        let output = cd(Path::new(from), &["--json", "cd", "-"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["path"],
            to.as_str()
        );
    }
    assert_eq!(fs::read_to_string(&directive).unwrap(), "");
    fixture.ok(&["rm", "first"]);
    let removed = cd(Path::new(&second_path), &["cd", "-"]);
    assert!(!removed.status.success());
    assert_eq!(fs::read_to_string(&directive).unwrap(), "");
}

#[test]
fn previous_workspace_is_remembered_per_command_and_not_for_scoped_callers() {
    let fixture = Fixture::new();
    fixture.add("first");
    fixture.add("second");
    fixture.ok(&["work", "status", "first"]);
    assert!(fixture.run(&["diff", "second"]).status.success());
    assert_eq!(
        fixture.ok(&["work", "status", "-"])["workspace"]["name"],
        "first"
    );
    let unsupported = fixture.run(&["work", "setup", "-"]);
    assert!(!unsupported.status.success());
    let binary = support::shoal_binary();
    let scoped = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "first",
            "--",
            binary,
            "--json",
            "work",
            "status",
            "-",
        ])
        .output()
        .unwrap();
    assert!(!scoped.status.success());
}

#[test]
fn menu_bindings_preserve_enter_status_cancel_and_scope() {
    let fixture = Fixture::new();
    let workspace = fixture.add("menu-worker");
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let picker = bin.join("fzf");
    for scoped in [false, true] {
        for key in ["", "ctrl-o", "ctrl-v", "ctrl-z", "cancel"] {
            install_test_script(
                &picker,
                format!(
                    r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/picker-args"
cat > "$HOME/picker-input"
[ '{key}' != cancel ] || exit 130
printf '%s\n' '{key}'
head -n 1 "$HOME/picker-input"
"#
                ),
            );
            let args = if scoped {
                vec![
                    "run",
                    "--workspace",
                    "menu-worker",
                    "--",
                    support::shoal_binary(),
                ]
            } else if key == "ctrl-o" {
                // `shoal work` opens the same picker when its output is a terminal.
                vec!["work"]
            } else {
                vec![]
            };
            fs::remove_file(fixture.root.path().join("picker-input")).ok();
            let listing = args == ["work"];
            let (output, transcript) =
                fixture.terminal_command(fixture.command().args(&args), "", listing);
            assert!(
                fixture.root.path().join("picker-input").exists(),
                "picker did not run: {transcript}"
            );
            let args = fs::read_to_string(fixture.root.path().join("picker-args")).unwrap();
            let input = fs::read_to_string(fixture.root.path().join("picker-input")).unwrap();
            assert!(input.contains(workspace["id"].as_str().unwrap()));
            assert_eq!(input.contains("Add workspace"), !scoped);
            // Every key is bound, and those the view leaves out unbound at start.
            let unbound = scoped.then_some("ctrl-t,ctrl-a,ctrl-s,ctrl-d");
            let binds: String = [
                "enter", "ctrl-t", "ctrl-f", "ctrl-v", "ctrl-a", "ctrl-e", "ctrl-o", "ctrl-s",
                "ctrl-d",
            ]
            .iter()
            .map(|key| format!("--bind\n{key}:print({key})+accept\n"))
            .collect();
            assert!(args.contains(&binds), "{args}");
            assert_eq!(args.contains("start:unbind("), unbound.is_some(), "{args}");
            if let Some(keys) = unbound {
                assert!(args.contains(&format!("start:unbind({keys})")), "{args}");
            }
            // Workspace processes have no inbox to switch to.
            assert_eq!(args.contains("tab:transform("), !scoped, "{args}");
            match key {
                "cancel" => {
                    assert_eq!(output.status.code(), Some(130), "{transcript}");
                }
                // An unknown key, and viewing a workspace that links nothing.
                "ctrl-z" | "ctrl-v" => {
                    assert!(!output.status.success(), "{transcript}");
                }
                _ => {
                    assert!(output.status.success(), "{transcript}");
                    let shown = String::from_utf8_lossy(&output.stdout) + transcript.as_str();
                    assert!(shown.contains("menu-worker"), "{shown}");
                }
            }
        }
    }
    // `--print` lists on a terminal instead of opening the picker, as a
    // redirected listing does.
    fs::remove_file(fixture.root.path().join("picker-input")).ok();
    fixture.interactive(&["work"], "");
    assert!(!fixture.root.path().join("picker-input").exists());
    let (output, transcript) = fixture.interactive(&["work", "--print"], "");
    assert!(output.status.success(), "{transcript}");
    assert!(!fixture.root.path().join("picker-input").exists());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("menu-worker"),
        "{output:?}"
    );
}

#[test]
fn bare_shoal_opens_on_the_inbox_and_switches_views() {
    let fixture = Fixture::new();
    let asker = fixture.add("asker");
    fixture.add("quiet");
    // Records its arguments and rows, then chooses the first row whose ID
    // starts with $PICK, or cancels when it is empty. `=ID` chooses a row
    // that was not in its input, as one a Tab reload brought is.
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/picker-args"
cat > "$HOME/picker-input"
[ -n "$PICK" ] || exit 130
printf '\n'
case "$PICK" in
=*) printf '%s\tloaded\n' "${PICK#=}" ;;
*) awk -F '\t' -v pick="$PICK" 'index($1, pick) == 1 { print; exit }' "$HOME/picker-input" ;;
esac
"#,
    );
    let read = |name: &str| fs::read_to_string(fixture.root.path().join(name)).unwrap();
    let open = |pick: &str, answer: &str| {
        let (output, transcript) =
            fixture.terminal_command(fixture.command().env("PICK", pick), answer, true);
        (
            output,
            transcript,
            read("picker-args"),
            read("picker-input"),
        )
    };

    // With nothing that needs the user, bare shoal shows the work going on.
    let (_, _, args, rows) = open("", "");
    assert!(args.contains("--prompt\nwork> \n"), "{args}");
    assert!(rows.contains(asker["id"].as_str().unwrap()), "{rows}");
    // An empty inbox still opens, so Tab can reach the other views.
    fixture.terminal_command(fixture.command().arg("inbox").env("PICK", ""), "", true);
    assert!(read("picker-args").contains("--prompt\ninbox> \n"));
    assert_eq!(read("picker-input"), "");

    fixture.ok(&[
        "questions",
        "ask",
        "Which database?",
        "--recommend",
        "SQLite",
        "--space",
        "asker",
    ]);
    let id = asker["id"].as_str().unwrap();
    let listed = fixture.run(&["internal", "menu-rows", "inbox"]);
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.starts_with(&format!("question:{id}:")), "{listed}");
    // Tab names the next view by the prompt fzf shows.
    let next = |prompt: &str| {
        let output = fixture
            .command()
            .env("FZF_PROMPT", prompt)
            .args(["internal", "menu-next"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    let to_work = next("inbox> ");
    assert!(
        to_work.contains("change-prompt(work> )")
            && !to_work.contains("unbind(")
            && to_work.ends_with("internal menu-rows work\n"),
        "{to_work}"
    );
    let to_notifications = next("work> ");
    assert!(
        to_notifications.starts_with("unbind(ctrl-t,ctrl-f,ctrl-v,ctrl-e,ctrl-s,ctrl-d)+")
            && to_notifications.ends_with("internal menu-rows notifications\n"),
        "{to_notifications}"
    );
    let to_inbox = next("notifications> ");
    assert!(
        to_inbox.starts_with("unbind(ctrl-e,ctrl-s,ctrl-d)+")
            && to_inbox.ends_with("internal menu-rows inbox\n"),
        "{to_inbox}"
    );
    // Notifications name their workspace, and showing them marks them read.
    fixture.ok(&["alerts", "send", "build finished", "--space", "asker"]);
    let shown = fixture.run(&["internal", "menu-rows", "notifications"]);
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(
        shown.starts_with("notification:asker\t") && shown.contains("build finished"),
        "{shown}"
    );
    assert_eq!(fixture.ok(&["alerts"]), serde_json::json!([]));
    // A workspace process cannot see them, even having cleared its token.
    fixture.ok(&["alerts", "send", "agent only", "--space", "asker"]);
    let cleared = fixture
        .command()
        .args(["run", "--workspace", "asker", "--", "sh", "-c"])
        // A second command keeps the scoped shell as the listing's parent.
        .arg(r#"env -u SHOAL_SCOPE_TOKEN "$0" internal menu-rows notifications; exit $?"#)
        .arg(support::shoal_binary())
        .output()
        .unwrap();
    assert!(!cleared.status.success(), "{cleared:?}");
    assert!(!String::from_utf8_lossy(&cleared.stdout).contains("agent only"));
    assert_eq!(
        fixture.ok(&["alerts"]).as_array().unwrap().len(),
        1,
        "the refused listing marked nothing read"
    );
    // Nor list the inbox without its token; with it, it gets its own items
    // printed instead of the picker it could not act from.
    let cleared = fixture
        .command()
        .args(["run", "--workspace", "asker", "--", "sh", "-c"])
        .arg(r#"env -u SHOAL_SCOPE_TOKEN "$0" inbox --print; exit $?"#)
        .arg(support::shoal_binary())
        .output()
        .unwrap();
    assert!(!cleared.status.success(), "{cleared:?}");
    assert!(!String::from_utf8_lossy(&cleared.stdout).contains("Which database?"));
    fs::remove_file(fixture.root.path().join("picker-input")).unwrap();
    let mut scoped = fixture.command();
    scoped.args([
        "run",
        "--workspace",
        "asker",
        "--",
        support::shoal_binary(),
        "inbox",
    ]);
    let (output, transcript) = fixture.terminal_command(&mut scoped, "", true);
    assert!(output.status.success(), "{transcript}");
    assert!(!fixture.root.path().join("picker-input").exists());
    assert!(transcript.contains("Which database?"), "{transcript}");

    // Inside a workspace with items, it starts searching for them.
    let path = Path::new(asker["path"].as_str().unwrap());
    fixture.interactive_command(fixture.command().current_dir(path).env("PICK", ""), "");
    assert!(read("picker-args").contains("--query\n'asker\n"));

    // Now bare shoal opens on the inbox, and Enter answers the question.
    let (output, transcript, args, rows) = open("question:", "Postgres\n");
    assert!(!args.contains("--query"), "{args}");
    assert!(output.status.success(), "{transcript}");
    assert!(args.contains("--prompt\ninbox> \n"), "{args}");
    assert!(
        args.contains("--bind\nstart:unbind(ctrl-e,ctrl-s,ctrl-d)\n"),
        "{args}"
    );
    assert!(args.contains("internal menu-next"), "{args}");
    assert!(rows.contains("Which database?"), "{rows}");
    // The question offers its recommendation as the default answer.
    assert!(transcript.contains("[SQLite]"), "{transcript}");
    assert_eq!(fixture.ok(&["inbox"]), serde_json::json!([]));
    // A row Tab loaded is chosen like the first view's: Enter on a
    // notification enters its workspace.
    let (output, transcript, _, _) = open("=notification:quiet", "");
    assert!(output.status.success(), "{transcript}");
    assert!(transcript.contains("quiet"), "{transcript}");
}

#[test]
fn inbox_decides_approvals_as_they_were_listed() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    let agent = fixture.add("agent");
    let pending = pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    // Chooses the row $PICK names; the regular prompt reads the decision.
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        r#"#!/bin/sh
cat > "$HOME/picker-input"
printf '\n'
case "$PICK" in
=*) printf '%s\tloaded\n' "${PICK#=}" ;;
*) awk -F '\t' -v pick="$PICK" 'index($1, pick) == 1 { print; exit }' "$HOME/picker-input" ;;
esac
"#,
    );
    let decide = |pick: &str| {
        let mut command = fixture.command();
        command.arg("inbox").env("PICK", pick);
        fixture.terminal_command(&mut command, "y\n", true)
    };
    let id = pending["number"].as_str().unwrap();
    let workspace = agent["id"].as_str().unwrap();
    // A request listed with another creation time is not the one shown.
    let (output, transcript) = decide(&format!("=approval:{workspace}:{id}:1"));
    assert!(!output.status.success(), "{output:?} {transcript}");
    assert_eq!(fixture.ok(&["approvals"])[0]["status"], "pending");
    let (output, transcript) = decide("approval:");
    assert!(output.status.success(), "{transcript}");
    assert!(transcript.contains("serve preview"));
    assert_eq!(fixture.ok(&["approvals"])[0]["status"], "approved");
}

#[test]
fn workspace_pickers_offer_all_workspaces_after_the_workspaces() {
    let fixture = Fixture::new();
    fixture.add("first");
    let (output, rows) = fixture.pick(&["leases", "hold"], "--all", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("first"), "{rows}");
    assert!(
        rows.lines()
            .last()
            .is_some_and(|row| row.starts_with("--all\t")),
        "{rows}"
    );
    // Stop leaves out workspaces with nothing running.
    let (output, rows) = fixture.pick(&["stop"], "", "");
    assert!(!output.status.success(), "{output:?}");
    assert!(rows.is_empty(), "{rows}");
}

#[test]
fn resume_picker_describes_what_each_workspace_would_restore() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let second = fixture.add("second");
    let idle = fixture.add("idle");
    let idle_path = Path::new(idle["path"].as_str().unwrap());
    // Nothing to resume: no picker.
    let (output, rows) = fixture.pick(&["resume"], "--all", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.is_empty(), "{rows}");
    // Every workspace is listed with what it would restore, stopped work first.
    fixture.save_stopped_command(&second);
    let (output, rows) = fixture.pick(&["resume"], second["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(
        rows[0].contains("second") && rows[0].contains("stopped: 1 command"),
        "{rows:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Stopped commands in second"),
        "{output:?}"
    );
    // A current workspace without stopped work falls back to the picker.
    fixture.save_stopped_command(&first);
    let (output, rows) = fixture.pick_in(idle_path, &["resume"], first["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rows.lines().count(), 3, "{rows}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Stopped commands in first"),
        "{output:?}"
    );
    // Several workspaces resume together, each with stopped work.
    fixture.save_stopped_command(&first);
    fixture.save_stopped_command(&second);
    let refused = fixture.run(&["resume", "first", "idle"]);
    assert!(!refused.status.success());
    let refused = fixture.run(&["resume", "first", "second", "--execution", "any"]);
    assert!(!refused.status.success());
    let picked = format!(
        "{} {}",
        first["id"].as_str().unwrap(),
        second["id"].as_str().unwrap()
    );
    let (output, _) = fixture.pick(&["resume"], &picked, "");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in ["first", "second"] {
        assert!(
            stdout.contains(&format!("Stopped commands in {name}")),
            "{stdout}"
        );
    }
}

#[test]
fn repository_commands_pick_an_omitted_repository() {
    let fixture = Fixture::new();
    let id = fixture.ok(&["repo", "ls"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let missing = fixture.run(&["repo", "rm", "--yes"]);
    assert!(!missing.status.success());

    let (output, rows) = fixture.pick(&["repo", "config"], &id, "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.starts_with(&format!("{id}\t")), "{rows:?}");
    let (output, _) = fixture.pick(&["repo", "rename"], &id, "renamed\n");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["repo", "ls"])[0]["name"], "renamed");
    let (output, _) = fixture.pick(&["repo", "rm", "--yes"], &id, "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["repo", "ls"]), serde_json::json!([]));
}

#[test]
fn omitted_repositories_come_from_the_current_checkout() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["branch", "topic"]);
    // No terminal: only the checkout can supply the repository.
    let run = |args: &[&str]| {
        fixture
            .command()
            .args(args)
            .current_dir(&fixture.repo)
            .output()
            .unwrap()
    };
    let output = run(&["repo", "config"]);
    assert!(output.status.success(), "{output:?}");
    let output = run(&["--json", "start", "--existing", "topic"]);
    assert!(output.status.success(), "{output:?}");
    for args in [&["start"][..], &["repo", "rename"]] {
        assert!(!run(args).status.success(), "{args:?}");
    }
    // Following the hints passes the branch and the name, not a repository.
    let repo = fixture.repo.to_str().unwrap();
    let output = run(&["--json", "start", repo, "fresh"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 2);
    let output = run(&["repo", "rename", repo, "renamed"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["repo", "ls"])[0]["name"], "renamed");
}

#[test]
fn base_set_picks_a_workspace_of_the_same_repository() {
    let fixture = Fixture::new();
    let lower = fixture.add("lower");
    fixture.add("upper");
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let foreign = fixture.ok(&["start", other.to_str().unwrap(), "foreign"]);
    let lower = lower["id"].as_str().unwrap();
    let (output, rows) = fixture.pick(
        &["branch", "base", "set", "--workspace", "upper"],
        lower,
        "",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains(&format!("{lower}\t")), "{rows:?}");
    assert_eq!(rows.lines().count(), 1, "{rows:?}");
    assert!(!rows.contains(foreign["id"].as_str().unwrap()));
    assert_eq!(
        fixture.ok(&["work", "status", "upper"])["workspace"]["base_workspace"]["name"],
        "lower"
    );
}

#[test]
fn work_lists_only_the_selected_repository() {
    let fixture = Fixture::new();
    fixture.add("local");
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "other\n")]);
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    fixture.ok(&["start", other.to_str().unwrap(), "foreign"]);
    let names = |repository: &str| -> Vec<_> {
        fixture
            .ok(&["work", "--repo", repository])
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].clone())
            .collect()
    };
    assert_eq!(names("other"), ["foreign"]);
    assert_eq!(names(fixture.repo.to_str().unwrap()), ["local"]);
    let unknown = fixture.run(&["work", "--repo", "missing"]);
    assert!(!unknown.status.success());
}

#[test]
fn work_lists_only_the_selected_statuses() {
    let fixture = Fixture::new();
    fixture.add("quiet");
    fixture.add("reviewed");
    fixture.ok(&["ready", "--space", "reviewed"]);
    let names = |statuses: &[&str]| -> Vec<_> {
        let mut args = vec!["work"];
        for status in statuses {
            args.extend(["--status", status]);
        }
        fixture
            .ok(&args)
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].clone())
            .collect()
    };
    assert_eq!(names(&["inactive"]), ["quiet"]);
    assert_eq!(names(&["ready"]), ["reviewed"]);
    assert_eq!(names(&["inactive,ready"]), ["quiet", "reviewed"]);
    assert_eq!(names(&["failed", "ready"]), ["reviewed"]);
    assert!(names(&["working"]).is_empty());
    let listed = String::from_utf8(fixture.run(&["work", "--status", "inactive"]).stdout).unwrap();
    assert!(
        listed.contains("quiet") && !listed.contains("reviewed"),
        "{listed}"
    );
    assert!(
        !fixture
            .run(&["work", "--status", "unknown"])
            .status
            .success()
    );
}

#[test]
fn inbox_lists_untracked_worktrees_until_they_are_adopted() {
    let fixture = Fixture::new();
    let workspace = fixture.add("tracked");
    let root = Path::new(workspace["path"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_owned();
    let stray = root.join("stray");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "stray",
            stray.to_str().unwrap(),
        ],
    );
    let untracked = |fixture: &Fixture| -> Vec<Value> {
        fixture
            .ok(&["inbox"])
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["kind"] == "untracked")
            .cloned()
            .collect()
    };
    // A listing skips a repository whose Git gate a background task holds,
    // so each read waits until the worktree is in it.
    let mut listed = Vec::new();
    wait_until("the untracked worktree in the inbox", || {
        listed = untracked(&fixture);
        !listed.is_empty()
    });
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(
        Path::new(listed[0]["worktree"]["path"].as_str().unwrap()),
        fs::canonicalize(&stray).unwrap()
    );
    wait_until("the untracked worktree in the inbox text", || {
        String::from_utf8(fixture.run(&["inbox"]).stdout)
            .unwrap()
            .contains("untracked")
    });
    // Workspace processes see only their own tree.
    let scoped = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "tracked",
            "--",
            support::shoal_binary(),
            "--json",
            "inbox",
        ])
        .output()
        .unwrap();
    assert!(scoped.status.success(), "{scoped:?}");
    assert!(!String::from_utf8_lossy(&scoped.stdout).contains("untracked"));

    fixture.ok(&["work", "adopt", stray.to_str().unwrap()]);
    assert!(untracked(&fixture).is_empty());
}

#[test]
fn inbox_lists_what_needs_the_user_by_kind() {
    let fixture = Fixture::new();
    set_repository_toml(&fixture, "[ports.web]\nrequires_approval=true\n");
    for name in ["reviewed", "waiter", "agent", "asker", "quiet"] {
        fixture.add(name);
    }
    assert_eq!(fixture.ok(&["inbox"]), serde_json::json!([]));
    assert_eq!(
        String::from_utf8(fixture.run(&["inbox"]).stdout).unwrap(),
        "Nothing needs you\n"
    );
    fixture.ok(&["ready", "--space", "reviewed"]);
    // The agent's state lasts while its execution runs, until `release`
    // exists; the loop's bound only keeps a failed test from leaving it behind.
    let release = fixture.root.path().join("release");
    let waiter_log = fixture.root.path().join("waiter.log");
    let mut waiter = fixture
        .command()
        .args(["run", "--workspace", "waiter", "--", "sh", "-c"])
        .arg(concat!(
            r#""$0" internal agent-state waiting && i=0 && "#,
            r#"until [ -e "$1" ] || [ $i -ge 3000 ]; do sleep 0.1; i=$((i + 1)); done"#
        ))
        .arg(support::shoal_binary())
        .arg(&release)
        .stdout(Stdio::null())
        .stderr(fs::File::create(&waiter_log).unwrap())
        .spawn()
        .unwrap();
    let mut last = Value::Null;
    let waiting = try_wait_for(|| {
        last = fixture.ok(&["inbox"]);
        (last.as_array().unwrap().len() == 2).then_some(())
    });
    if waiting.is_none() {
        let status = waiter.try_wait();
        let log = fs::read_to_string(&waiter_log).unwrap_or_default();
        panic!("timed out waiting for the waiting agent: waiter {status:?} {log}, inbox {last}");
    }
    pending_approval(scoped_command(
        &fixture,
        "agent",
        &["acquire", "port", "web", "--reason", "serve preview"],
    ));
    fixture.ok(&[
        "questions",
        "ask",
        "Which database?",
        "--recommend",
        "SQLite",
        "--space",
        "asker",
    ]);

    let items = fixture.ok(&["inbox"]);
    let listed: Vec<_> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["question", "approval", "needs_input", "ready"]);
    assert_eq!(items[0]["question"]["space"], "asker");
    assert_eq!(items[1]["request"]["space"], "agent");
    assert_eq!(items[2]["workspace"]["name"], "waiter");
    let text = String::from_utf8(fixture.run(&["inbox"]).stdout).unwrap();
    let rows: Vec<_> = text.lines().collect();
    assert_eq!(rows.len(), 4, "{text}");
    let (question, request) = (
        &items[0]["question"]["number"],
        &items[1]["request"]["number"],
    );
    assert!(
        rows[0].starts_with(&format!("question {}  asker ", question.as_str().unwrap()))
            && rows[0].contains("Which database?")
            && rows[0].ends_with("SQLite"),
        "{text}"
    );
    assert!(
        rows[1].starts_with(&format!("approval {}  agent ", request.as_str().unwrap()))
            && rows[1].contains("serve preview"),
        "{text}"
    );
    assert_eq!(rows[2].trim_end(), "needs input   waiter");
    assert_eq!(rows[3].trim_end(), "ready         reviewed", "{text}");
    assert!(
        !fixture
            .run(&["inbox", "--repo", "missing"])
            .status
            .success()
    );
    fs::write(&release, "").unwrap();
    assert!(waiter.wait().unwrap().success());
}

#[test]
fn hold_release_picks_an_omitted_hold() {
    let fixture = Fixture::new();
    fixture.add("upper");
    let none = fixture.run(&["release", "hold", "--workspace", "upper"]);
    assert!(!none.status.success());
    fixture.ok(&[
        "acquire",
        "hold",
        "--workspace",
        "upper",
        "--name",
        "review",
    ]);
    fixture.ok(&[
        "acquire",
        "hold",
        "--workspace",
        "upper",
        "--name",
        "demo",
        "--reason",
        "show",
    ]);
    let (output, rows) = fixture.pick(&["release", "hold", "--workspace", "upper"], "demo", "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.contains("demo\tdemo (show)") && rows.contains("review\t"));
    let holds = fixture.ok(&["leases", "hold", "--workspace", "upper"]);
    assert_eq!(holds.as_array().unwrap().len(), 1);
    assert_eq!(holds[0]["name"], "review");
}

#[test]
fn config_show_picks_a_workspace_outside_any_checkout() {
    let fixture = Fixture::new();
    let workspace = fixture.add("configured");
    let id = workspace["id"].as_str().unwrap();
    let (output, rows) = fixture.pick(&["config", "show"], id, "");
    assert!(output.status.success(), "{output:?}");
    assert!(rows.starts_with(&format!("{id}\t")), "{rows:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("agent.default"));
}

#[test]
fn cd_always_picks_even_inside_a_workspace_and_cancel_does_not_navigate() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let second = fixture.add("second");
    let mut broken = fixture.command();
    let failed = broken
        .args([
            "start",
            fixture.repo.to_str().unwrap(),
            "missing",
            "--base",
            "not-a-ref",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let bin = fixture.root.path().join("bin");
    let fzf = bin.join("fzf");
    install_test_script(
        &fzf,
        "#!/bin/sh\ncat > \"$SHOAL_TEST_PICK_INPUT\"\n[ \"$SHOAL_TEST_PICK_ID\" != cancel ] || exit 130\nawk -F '\\t' -v id=\"$SHOAL_TEST_PICK_ID\" '$1 == id { print }' \"$SHOAL_TEST_PICK_INPUT\"\n",
    );
    let directive = fixture.root.path().join("cd-directive");
    let input = fixture.root.path().join("picker-input");
    for choice in [second["id"].as_str().unwrap(), "cancel"] {
        fs::write(&directive, "").unwrap();
        let (_master, slave) = pty::open();
        let output = fixture
            .command()
            .current_dir(first["path"].as_str().unwrap())
            .env("SHOAL_TEST_PICK_INPUT", &input)
            .env("SHOAL_TEST_PICK_ID", choice)
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .arg("cd")
            .stdin(slave.try_clone().unwrap())
            .stderr(slave)
            .output()
            .unwrap();
        let choices = fs::read_to_string(&input).unwrap();
        assert!(choices.contains(first["id"].as_str().unwrap()));
        assert!(choices.contains(second["id"].as_str().unwrap()));
        assert!(!choices.contains("missing"));
        if choice == "cancel" {
            assert!(!output.status.success());
            assert_eq!(fs::read_to_string(&directive).unwrap(), "");
        } else {
            assert!(output.status.success());
            assert_eq!(
                fs::read_to_string(&directive).unwrap().trim(),
                second["path"].as_str().unwrap()
            );
        }
    }
    // JSON/piped invocations must not silently choose the current workspace.
    let output = fixture
        .command()
        .current_dir(first["path"].as_str().unwrap())
        .args(["--json", "cd"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn the_inbox_lists_untracked_worktrees_under_shoal_directories_only() {
    let fixture = Fixture::new();
    let owned = fixture.add("owned");
    let owned_path = Path::new(owned["path"].as_str().unwrap());
    let orphan = owned_path.parent().unwrap().join("nested/orphan");
    let outside = fixture.root.path().join("outside");
    for (path, branch) in [(&orphan, "orphan"), (&outside, "outside")] {
        git(
            &fixture.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
    }
    fs::write(orphan.join("dirty"), "keep me").unwrap();
    // A listing skips a repository whose Git gate a background task holds.
    let mut paths = Vec::new();
    wait_until("the untracked worktree in the inbox", || {
        paths = fixture
            .ok(&["inbox"])
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["kind"] == "untracked")
            .map(|item| item["worktree"]["path"].as_str().unwrap().to_owned())
            .collect();
        !paths.is_empty()
    });
    assert_eq!(
        paths,
        [fs::canonicalize(&orphan).unwrap().to_str().unwrap()]
    );
    assert_eq!(fs::read_to_string(orphan.join("dirty")).unwrap(), "keep me");
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
}

#[test]
fn outside_a_checkout_issue_open_picks_a_workspace_and_repo_commands_a_repository() {
    let fixture = Fixture::new();
    let workspace = fixture.add("linked");
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let offered = |args: &[&str]| {
        // No row matches the choice, so the picker cancels.
        let (_, rows) = fixture.pick(args, "none", "\n");
        rows.lines()
            .map(|row| row.split('\t').next().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    // `--link` links the new issue to the workspace picked.
    assert_eq!(
        offered(&["issue", "open", "--title", "Bug", "--link"]),
        [workspace["id"].as_str().unwrap()]
    );
    for args in [
        &["repo", "labels"][..],
        &["issue", "ls"],
        &["forge", "reactions"],
    ] {
        assert_eq!(
            offered(args),
            [repository["id"].as_str().unwrap()],
            "{args:?}"
        );
    }
}
