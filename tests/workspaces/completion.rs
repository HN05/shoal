//! Completing work: `shoal done`, completion cleanup, and the agent exit
//! and post-done hooks.
use super::creation::issue_fixture;
use super::forge::wait_removed;
use super::*;

#[test]
fn done_records_a_summary_that_status_shows() {
    // Completion without cleanup keeps the workspace to read.
    let fixture = Fixture::with_config(Some("[cleanup.done]\nenabled = false\n"));
    fixture.add("audit");
    let refused = fixture.run(&["done", "audit", "--summary", "two\nlines"]);
    assert!(!refused.status.success());
    assert!(fixture.ok(&["work", "status", "audit"])["completion"].is_null());
    fixture.ok(&["done", "audit", "--summary", "No leak"]);
    let status = fixture.ok(&["work", "status", "audit"]);
    assert_eq!(status["completion"]["summary"], "No leak", "{status}");
}

fn install_done_hook(fixture: &Fixture) -> PathBuf {
    let hook = fixture.root.path().join("done hook.sh");
    install_test_script(
        &hook,
        "#!/bin/sh\nprintf '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\\n' \"$SHOAL_HOOK\" \"$SHOAL_WORKSPACE\" \"$SHOAL_WORKSPACE_ID\" \"$SHOAL_WORKSPACE_PATH\" \"$PWD\" \"$SHOAL_STATE_DIR\" \"$SHOAL_DONE_CHOICE\" \"${SHOAL_SCOPE_TOKEN-unset}\" \"${SHOAL_EXECUTION_ID-unset}\" \"${SHOAL_RESERVED_PORT_ENV-unset}\" >> \"$HOME/done-events\"\n",
    );
    hook
}

#[test]
fn agent_exit_hook_exposes_custom_agent_results_without_completing_the_assignment() {
    let fixture = Fixture::with_config(Some("hooks.post_agent_exit = '/usr/bin/false'\n"));
    let hook = fixture.root.path().join("agent-exit.sh");
    install_test_script(
        &hook,
        "#!/bin/sh\nprintf '%s|%s|%s|%s|%s|%s|%s\\n' \"$SHOAL_HOOK\" \"$SHOAL_WORKSPACE\" \"$SHOAL_AGENT\" \"$SHOAL_AGENT_EXIT_CODE\" \"$SHOAL_AGENT_EXIT_COMPLETE\" \"${SHOAL_SCOPE_TOKEN-unset}\" \"${SHOAL_DONE_CHOICE-unset}\" >> \"$HOME/agent-exits\"\n",
    );
    set_repository_toml(
        &fixture,
        &format!(
            "hooks.post_agent_exit = '{}'\n[agents.helper]\ncommand = ['/bin/sh', '-c', 'exit 7']\n\
             [commands]\nfailing = ['/bin/sh', '-c', 'exit 7']\n",
            hook.display()
        ),
    );
    let agent = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "agent-work",
        "--here",
        "--agent",
        "helper",
    ]);
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    let events = fixture.root.path().join("agent-exits");
    wait_until("agent exit hook", || {
        fs::read_to_string(&events).is_ok_and(|events| {
            events.contains("post_agent_exit|agent-work|helper|7|true|unset|unset\n")
        })
    });
    assert_eq!(
        fs::read_to_string(&events).unwrap(),
        "post_agent_exit|agent-work|helper|7|true|unset|unset\n"
    );
    assert!(fixture.ok(&["work", "status", "agent-work"])["completion"].is_null());
    assert_eq!(
        fixture
            .run(&["run", "failing", "--workspace", "agent-work"])
            .status
            .code(),
        Some(7)
    );
    assert_eq!(fs::read_to_string(&events).unwrap().lines().count(), 1);
    // Falling back to a failing global hook must preserve the agent's code.
    fixture.ok(&[
        "config",
        "unset",
        "hooks.post_agent_exit",
        "--repo",
        fixture.repo.to_str().unwrap(),
    ]);
    let agent = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "agent-work",
        "--here",
        "--agent",
        "helper",
    ]);
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    wait_until("failed agent exit hook", || {
        fixture
            .ok(&["alerts", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "hook_failed" && n["workspace"] == "agent-work")
    });
}

#[test]
fn agent_exit_hook_can_signal_completion_without_a_post_done_hook() {
    let fixture = Fixture::new();
    let hook = fixture.root.path().join("finish-assignment");
    install_test_script(
        &hook,
        format!(
            "#!/bin/sh\nexec '{}' --json done \"$SHOAL_WORKSPACE\"\n",
            support::shoal_binary()
        ),
    );
    set_repository_toml(
        &fixture,
        &format!(
            "hooks.post_agent_exit = '{}'\n[cleanup.done]\nenabled = false\n[agents.helper]\ncommand = ['/bin/sh', '-c', 'exit 7']\n",
            hook.display()
        ),
    );
    let agent = fixture.run(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "agent-work",
        "--here",
        "--agent",
        "helper",
    ]);
    assert_eq!(agent.status.code(), Some(7), "{agent:?}");
    wait_until("completion from exit hook", || {
        fixture.ok(&["work", "status", "agent-work"])["completion"]["cleanup"] == false
    });
}

#[test]
fn agent_exit_acknowledgement_does_not_wait_for_the_hook() {
    let fixture = Fixture::new();
    let hook = fixture.root.path().join("exit-hook");
    let barrier = fixture.root.path().join("release-exit");
    assert!(
        Command::new("mkfifo")
            .arg(&barrier)
            .status()
            .unwrap()
            .success()
    );
    install_test_script(
        &hook,
        "#!/bin/sh\ntouch \"$HOME/exit-started\"\nread release < \"$HOME/release-exit\"\ntouch \"$HOME/exit-finished\"\n",
    );
    set_repository_toml(
        &fixture,
        &format!(
            "hooks.post_agent_exit = '{}'\n[agents.helper]\ncommand = ['/bin/sh', '-c', 'exit 7']\n",
            hook.display()
        ),
    );
    let mut agent = fixture
        .command()
        .args([
            "start",
            fixture.repo.to_str().unwrap(),
            "agent-work",
            "--here",
            "--agent",
            "helper",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("agent exit hook started", || {
        fixture.root.path().join("exit-started").exists()
    });
    let mut status = None;
    wait_until("agent wrapper acknowledged", || {
        status = agent.try_wait().unwrap();
        status.is_some()
    });
    assert_eq!(status.unwrap().code(), Some(7));
    assert!(!fixture.root.path().join("exit-finished").exists());
    fs::write(&barrier, "release\n").unwrap();
    wait_until("agent exit hook finished", || {
        fixture.root.path().join("exit-finished").exists()
    });
}

#[test]
fn post_done_hook_runs_for_scoped_done_and_retained_cleanup_with_config_layers() {
    let mut fixture = Fixture::with_config(Some("hooks.post_done = '/usr/bin/false'\n"));
    let workspace = fixture.add("hooked");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let hook = install_done_hook(&fixture);
    let config = format!("hooks.post_done = '{}'\n", hook.display());
    // The worktree file overrides the global failure hook.
    fs::write(
        path.join(".shoal.toml"),
        format!("{config}[cleanup.done]\nenabled = false\n"),
    )
    .unwrap();
    let completion = fixture.ok(&[
        "run",
        "--workspace",
        "hooked",
        "--",
        support::shoal_binary(),
        "--json",
        "done",
    ]);
    assert_eq!(completion["cleanup"], false);
    let events = fixture.root.path().join("done-events");
    wait_until("keep hook", || events.exists());
    let identity = format!(
        "post_done|hooked|{}|{}|{}|{}|",
        workspace["id"].as_str().unwrap(),
        path.display(),
        path.display(),
        fixture.root.path().join("state").display()
    );
    assert_eq!(
        fs::read_to_string(&events).unwrap(),
        format!("{identity}keep|unset|unset|unset\n")
    );
    // Saved repository config overrides the file and determines the default choice.
    fs::write(
        path.join(".shoal.toml"),
        "hooks.post_done = '/usr/bin/false'\n",
    )
    .unwrap();
    set_repository_toml(&fixture, &format!("{config}[cleanup.done]\nenabled=true\n"));
    // Retention must survive removal of the configuration file later in this
    // test; otherwise the cleanup sweep can win the race against the last done.
    fs::write(path.join("retain-work"), "uncommitted work\n").unwrap();
    fixture.ok(&["done", "hooked"]);
    wait_until("completion cleanup error", || {
        fixture.ok(&["work", "status", "hooked"])["completion"]["error"].is_string()
    });
    assert_eq!(
        fs::read_to_string(&events).unwrap(),
        format!("{identity}keep|unset|unset|unset\n{identity}cleanup|unset|unset|unset\n")
    );
    fixture.restart();
    assert_eq!(fs::read_to_string(&events).unwrap().lines().count(), 2);
    // Failure is best effort and cannot cancel a recorded choice.
    fixture.clear_local_config(fixture.repo.to_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[cleanup.done]\nenabled = false\n",
    )
    .unwrap();
    assert_eq!(fixture.ok(&["done", "hooked"])["cleanup"], false);
    assert!(
        fixture
            .ok(&["alerts", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "hook_failed" && n["workspace"] == "hooked")
    );
}

#[test]
fn post_done_hook_observes_persisted_completion_and_excludes_lifecycle_and_permits() {
    let fixture = Fixture::with_config(Some("[resources.device]\n"));
    fixture.add("hooked");
    let hook = install_done_hook(&fixture);
    let barrier = fixture.root.path().join("release-done");
    assert!(
        Command::new("mkfifo")
            .arg(&barrier)
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        &hook,
        "#!/bin/sh\ntouch \"$HOME/done-started\"\nread release < \"$HOME/release-done\"\n",
    )
    .unwrap();
    set_repository_toml(
        &fixture,
        &format!(
            "hooks.post_done = '{}'\nsetup.command = '/usr/bin/true'\n[cleanup.done]\nenabled = false\n",
            hook.display()
        ),
    );
    let mut done = fixture
        .command()
        .args(["done", "hooked"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("post-done hook started", || {
        fixture.root.path().join("done-started").exists()
    });
    assert_eq!(
        fixture.ok(&["work", "status", "hooked"])["completion"]["cleanup"],
        false
    );
    for args in [
        vec!["work", "setup", "hooked"],
        vec!["rm", "hooked", "--yes"],
        vec!["acquire", "resource", "device", "--workspace", "hooked"],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
    }
    fs::write(&barrier, "release\n").unwrap();
    assert!(done.wait().unwrap().success());
    fixture.ok(&["acquire", "resource", "device", "--workspace", "hooked"]);
}

#[test]
fn done_defaults_to_cleanup_and_releases_workspace_resources() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n[resources.device]\n"));
    let hook = install_done_hook(&fixture);
    let workspace = fixture.add("finished");
    set_repository_toml(
        &fixture,
        &format!("hooks.post_done = '{}'\n", hook.display()),
    );
    fixture.ok(&["acquire", "port", "web", "--workspace", "finished"]);
    fixture.ok(&["acquire", "resource", "device", "--workspace", "finished"]);
    let mut wrapper = fixture
        .command()
        .args(["run", "--workspace", "finished", "--"])
        .args(READABLE_SLEEP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "finished");
    let completion = fixture.ok(&["done", "finished"]);
    assert_eq!(completion["cleanup"], true);
    wait_removed(&fixture, "finished");
    assert!(
        fs::read_to_string(fixture.root.path().join("done-events"))
            .unwrap()
            .contains("|cleanup|")
    );
    assert!(!wrapper.wait().unwrap().success());
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert!(
        fixture
            .ok(&["leases", "resource", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .ok(&["leases", "port", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn agents_follow_a_keep_default_and_can_request_cleanup() {
    let fixture = Fixture::with_config(Some("[cleanup.done]\nenabled=false\n"));
    fixture.add("keep-default");
    assert_eq!(fixture.ok(&["done", "keep-default"])["cleanup"], false);
    // Agents can explicitly clean their own workspace despite a keep default.
    fixture.run(&[
        "run",
        "--workspace",
        "keep-default",
        "--",
        support::shoal_binary(),
        "done",
        "--cleanup",
    ]);
    wait_removed(&fixture, "keep-default");
}

#[test]
fn done_records_completion_without_claiming_dirty_work_is_merged() {
    let fixture = Fixture::new();
    let workspace = fixture.add("unfinished");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("work"), "retain").unwrap();
    fixture.ok(&["done", "unfinished"]);
    wait_until("completion cleanup error", || {
        fixture.ok(&["work", "status", "unfinished"])["completion"]["error"].is_string()
    });
    assert_eq!(fs::read_to_string(path.join("work")).unwrap(), "retain");
    assert!(fixture.ok(&["work", "status", "unfinished"])["pr_cleanup"].is_null());
}

#[test]
fn multiple_prs_keep_a_done_workspace_without_done_cleanup() {
    multiple_prs_clean_up_only_after_every_merge_and_honor_done_policy(false);
}

#[test]
fn multiple_prs_clean_up_only_after_every_merge() {
    multiple_prs_clean_up_only_after_every_merge_and_honor_done_policy(true);
}

/// Merge a done workspace's PRs one by one, with done cleanup enabled when
/// `cleanup`: the workspace goes only after every merge, and stays without
/// done cleanup.
fn multiple_prs_clean_up_only_after_every_merge_and_honor_done_policy(cleanup: bool) {
    let mut fixture = Fixture::with_config(Some(&format!(
        "[cleanup.auto]\nenabled=false\n[cleanup.done]\nenabled={cleanup}\n"
    )));
    let hook = install_done_hook(&fixture);
    set_repository_toml(
        &fixture,
        &format!("hooks.post_done = '{}'\n", hook.display()),
    );
    let workspace = fixture.add("multi");
    fixture.add_github_origin();
    let head = git(
        Path::new(workspace["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    );
    let earlier = "e".repeat(40);
    let prs = std::cell::RefCell::new(serde_json::Map::new());
    let write_pr = |fixture: &Fixture, number: u64, state: &str, commit: &str| {
        let pull = format!("repos/team/project/pulls/{number}");
        let mut prs = prs.borrow_mut();
        prs.insert(
            format!("GET {pull}"),
            serde_json::json!({"body": {"number": number, "title": "PR",
                "state": if state == "open" { "open" } else { "closed" },
                "merged": state == "merged", "labels": [],
                "head": {"ref": "multi"}, "base": {"ref": "main"}}}),
        );
        prs.insert(
            format!("GET {pull}/commits?per_page=100&page=1"),
            serde_json::json!({"body": [{"sha": commit}]}),
        );
        fixture.script_github(&prs);
    };
    write_pr(&fixture, 1, "open", &earlier);
    write_pr(&fixture, 2, "open", head.trim());
    fixture.ok(&["link", "pr", "1", "assigned", "--space", "multi"]);
    fixture.ok(&["link", "pr", "2", "assigned", "--space", "multi"]);
    fixture.ok(&["link", "pr", "1", "assigned", "--space", "multi"]);
    assert_eq!(
        fixture.ok(&["work", "status", "multi"])["pr_cleanup"]["urls"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // Scoped cancellation selects one PR and preserves the other.
    fixture.ok(&[
        "run",
        "--workspace",
        "multi",
        "--",
        support::shoal_binary(),
        "--json",
        "unlink",
        "pr",
        "2",
        "assigned",
    ]);
    assert_eq!(
        fixture.ok(&["work", "status", "multi"])["pr_cleanup"]["url"],
        "https://github.com/team/project/pull/1"
    );
    fixture.ok(&[
        "run",
        "--workspace",
        "multi",
        "--",
        support::shoal_binary(),
        "--json",
        "link",
        "pr",
        "2",
        "assigned",
    ]);
    fixture.ok(&["done", "multi"]);
    write_pr(&fixture, 1, "merged", &earlier);
    write_pr(&fixture, 2, "closed", head.trim());
    let root = fixture.root.path().to_owned();
    let queries = root.join("forge/requests.jsonl");
    fs::write(&queries, "").unwrap();
    fixture.restart();
    wait_until("second PR queried", || {
        fs::read_to_string(&queries)
            .unwrap()
            .contains("GET repos/team/project/pulls/2\"")
    });
    assert!(fixture.ok(&["work", "status", "multi"])["pr_cleanup"]["merged_head"].is_null());
    write_pr(&fixture, 2, "merged", head.trim());
    fixture.restart();
    if cleanup {
        wait_removed(&fixture, "multi");
    } else {
        wait_until("confirmed merges", || {
            fixture.ok(&["work", "status", "multi"])["pr_cleanup"]["merged_head"] == head.trim()
        });
        let state = fixture.ok(&["work", "status", "multi"]);
        assert_eq!(state["pr_cleanup"]["merged_head"], head.trim());
        fixture.ok(&["alerts"]);
        // A changed default must not change a kept assignment.
        fs::write(
            root.join(".config/shoal/config.toml"),
            "[cleanup.auto]\nenabled=false\n[cleanup.done]\nenabled=true\n",
        )
        .unwrap();
        fixture.restart();
        assert_eq!(
            fixture.ok(&["work", "status", "multi"])["completion"]["cleanup"],
            false
        );
    }
    let db = rusqlite::Connection::open(root.join("state/state.db")).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM notifications WHERE kind='workspace_done' AND workspace='multi'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        fs::read_to_string(root.join("done-events"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn scoped_undone_withdraws_completion_without_keeping_the_workspace() {
    let fixture = issue_fixture("github.com");
    let scoped = |fixture: &Fixture, args: &[&str]| {
        let mut command = vec![
            "run",
            "--workspace",
            "issue-work",
            "--",
            support::shoal_binary(),
            "--json",
        ];
        command.extend_from_slice(args);
        fixture.ok(&command)
    };
    fixture.ok(&[
        "acquire",
        "hold",
        "--workspace",
        "issue-work",
        "--name",
        "review",
    ]);
    scoped(&fixture, &["done"]);
    assert_eq!(
        fixture.ok(&["work", "status", "issue-work"])["workspace"]["done"],
        true
    );
    let undone = scoped(&fixture, &["done", "--undo"]);
    assert_eq!(undone["withdrawn"]["cleanup"], true);
    let refused = fixture.run(&[
        "run",
        "--workspace",
        "issue-work",
        "--",
        support::shoal_binary(),
        "done",
        "--undo",
    ]);
    assert!(!refused.status.success());
    let inspection = fixture.ok(&["work", "status", "issue-work"]);
    assert!(inspection["completion"].is_null());
    assert_eq!(inspection["workspace"]["done"], false);
    fixture.ok(&[
        "release",
        "hold",
        "--workspace",
        "issue-work",
        "--name",
        "review",
    ]);
    // Only holds keep a workspace; cleanup removes the merged one on request,
    // which needs a default branch it can fetch.
    git(&fixture.repo, &["remote", "remove", "origin"]);
    assert_eq!(
        fixture.ok(&["cleanup", "spaces"])["removed"],
        serde_json::json!(["issue-work"])
    );
}
