use crate::support::{Fixture, git, wait_registered_execution, wait_removed};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

fn wait_pr_error(fixture: &Fixture, name: &str, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let inspection = fixture.ok(&["inspect", name]);
        if inspection["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|s| s.contains(message))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {message}: {inspection}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn merged_stops_agent_and_releases_resources_without_idle_delay() {
    let fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n[resources.device]\n"));
    let workspace = fixture.add("merged");
    fixture.ok(&["port", "acquire", "web", "merged"]);
    fixture.ok(&["resource", "acquire", "device", "merged"]);
    let mut wrapper = fixture
        .command()
        .args(["exec", "merged", "--", "sleep", "60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "merged");
    fixture.ok(&["pr", "merged", "merged"]);
    wait_removed(&fixture, "merged");
    assert!(!wrapper.wait().unwrap().success());
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert!(
        fixture
            .ok(&["resource", "list", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .ok(&["port", "list", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn merged_retains_dirty_work_and_changed_head_across_restart_and_can_be_cancelled() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("retain");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("dirty"), "keep").unwrap();
    fixture.ok(&[
        "exec",
        "retain",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "pr",
        "merged",
    ]);
    wait_pr_error(&fixture, "retain", "uncommitted");
    git(path, &["add", "dirty"]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "new work",
        ],
    );
    fixture.restart();
    wait_pr_error(&fixture, "retain", "HEAD changed");
    assert!(path.join("dirty").exists());
    fixture.ok(&["pr", "clear", "retain"]);
    assert!(fixture.ok(&["inspect", "retain"])["pr_cleanup"].is_null());
}

#[test]
fn pr_cleanup_can_be_disabled_independently() {
    let fixture = Fixture::with_config(Some("[pr_cleanup]\nenabled=false\n"));
    fixture.add("keep");
    let output = fixture.run(&["pr", "merged", "keep"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("PR cleanup is disabled"));
    assert!(fixture.ok(&["inspect", "keep"])["pr_cleanup"].is_null());
}

#[test]
fn repository_config_sets_pr_cleanup_over_the_global_default() {
    let fixture = Fixture::with_config(Some("[pr_cleanup]\nenabled=false\n"));
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[pr_cleanup]\nenabled=true\n",
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
            "enable pr cleanup",
        ],
    );
    fixture.add("merged");
    fixture.ok(&["pr", "merged", "merged"]);
    wait_removed(&fixture, "merged");
    // The saved config is the top layer.
    let saved = fixture.root.path().join("saved.toml");
    fs::write(&saved, "[pr_cleanup]\nenabled=false\n").unwrap();
    fixture.ok(&[
        "repo",
        "config",
        fixture.repo.to_str().unwrap(),
        "--file",
        saved.to_str().unwrap(),
    ]);
    fixture.add("kept");
    let output = fixture.run(&["pr", "merged", "kept"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("PR cleanup is disabled"));
}

#[test]
fn pr_watch_checks_github_state_and_commit_and_survives_restart() {
    let mut fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n"));
    let workspace = fixture.add("watch");
    let failed = fixture.run(&["pr", "56", "watch"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("origin remote"));
    assert!(fixture.ok(&["inspect", "watch"])["pr_cleanup"].is_null());
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/team/repo.git",
        ],
    );
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("gh"), "#!/bin/sh\ncat \"$HOME/pr.json\"\n").unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let failed = fixture.run(&["pr", "https://github.com/team/repo/pull/56", "watch"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("shoal pr merged"));
    assert!(fixture.ok(&["inspect", "watch"])["pr_cleanup"].is_null());
    assert!(
        !fixture
            .run(&["pr", "https://github.com/other/repo/pull/56", "watch"])
            .status
            .success()
    );
    let response = fixture.root.path().join("pr.json");
    let write_response = |state: &str, head: &str| {
        fs::write(&response, serde_json::json!({"number":56,"state":state,"headRefName":"watch","commits":[{"oid":head}]}).to_string()).unwrap()
    };
    let head = git(
        Path::new(workspace["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    )
    .trim()
    .to_owned();
    write_response("OPEN", &head);
    fixture.ok(&["pr", "https://github.com/team/repo/pull/56", "watch"]);
    fixture.ok(&["pr", "clear", "watch"]);
    fixture.ok(&["pr", "56", "watch"]);
    fixture.ok(&["pr", "clear", "watch"]);
    // Scoped callers can omit the workspace and register by number too.
    fixture.ok(&[
        "exec",
        "watch",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "--json",
        "pr",
        "56",
    ]);
    fixture.restart();
    assert_eq!(
        fixture.ok(&["inspect", "watch"])["pr_cleanup"]["url"],
        "https://github.com/team/repo/pull/56"
    );
    // A persisted number must not silently follow a changed origin.
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/other/repo.git",
        ],
    );
    fixture.restart();
    wait_pr_error(&fixture, "watch", "different repository");
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/team/repo.git",
        ],
    );
    write_response("MERGED", &"a".repeat(40));
    fixture.restart();
    wait_pr_error(&fixture, "watch", "does not contain");
    fs::write(&response, "malformed output").unwrap();
    fixture.restart();
    wait_pr_error(&fixture, "watch", "expected");
    write_response("CLOSED", &head);
    fixture.restart();
    assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    write_response("MERGED", &head);
    fixture.restart();
    wait_removed(&fixture, "watch");
}

#[test]
fn pr_watch_checks_forgejo_merge_and_commits_with_fixture_cli() {
    let fixture = Fixture::new();
    let workspace = fixture.add("fj-watch");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("fj"), "#!/bin/sh\nfor arg; do last=$arg; done\nif [ \"$last\" = commits ]; then cat \"$HOME/commits\"; else printf 'Title #56\\nBy user — Merged — +1 -0\\nFrom `fj-watch` into `main`\\n'; fi\n").unwrap();
    fs::set_permissions(bin.join("fj"), fs::Permissions::from_mode(0o755)).unwrap();
    let head = git(
        Path::new(workspace["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    );
    fs::write(
        fixture.root.path().join("commits"),
        format!("commit {} (+1, -0)\nAuthor: Test\n", head.trim()),
    )
    .unwrap();
    fixture.ok(&["pr", "56", "fj-watch"]);
    wait_removed(&fixture, "fj-watch");
}

#[test]
fn merged_rechecks_head_after_removal_hooks() {
    for key in ["pre_remove_cmd", "pre_resource_release_cmd"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.repo.join(".shoal.toml"),
            format!("{key} = 'hook.sh'\n[resources.signing]\n"),
        )
        .unwrap();
        fs::write(fixture.repo.join("hook.sh"), "#!/bin/sh\ngit -c user.name=Test -c user.email=test@example.invalid commit --allow-empty -m 'hook work'\n").unwrap();
        fs::set_permissions(
            fixture.repo.join("hook.sh"),
            fs::Permissions::from_mode(0o755),
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
                "hook",
            ],
        );
        let workspace = fixture.add("hook");
        if key == "pre_resource_release_cmd" {
            fixture.ok(&["resource", "acquire", "signing", "hook"]);
        }
        fixture.ok(&["pr", "merged", "hook"]);
        wait_pr_error(&fixture, "hook", "HEAD changed during removal hooks");
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    }
}

#[test]
fn legacy_pr_registrations_survive_disabled_cleanup_and_clear_after_restart() {
    let mut fixture = Fixture::with_config(Some(
        "[pr_cleanup]\nenabled=false\n[auto_cleanup]\nenabled=false\n",
    ));
    for name in ["watch", "acknowledged"] {
        let workspace = fixture.add(name);
        let head = git(
            Path::new(workspace["path"].as_str().unwrap()),
            &["rev-parse", "HEAD"],
        );
        let record = if name == "watch" {
            serde_json::json!({"url": "https://forge.example/team/repo/pulls/7", "head": null, "error": "previous lookup failure"})
        } else {
            serde_json::json!({"url": null, "head": head.trim(), "error": null})
        };
        let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
        db.execute(
            "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
            rusqlite::params![workspace["id"].as_str().unwrap(), record.to_string()],
        )
        .unwrap();
        fixture.restart();
        assert_eq!(fixture.ok(&["inspect", name])["pr_cleanup"], record);
        for command in ["merged", "7"] {
            let output = fixture.run(&["pr", command, name]);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("PR cleanup is disabled"));
        }
        assert_eq!(fixture.ok(&["inspect", name])["pr_cleanup"], record);
        // Clear remains allowed while disabled, including for a scoped caller.
        assert_eq!(
            fixture.ok(&[
                "exec",
                name,
                "--",
                env!("CARGO_BIN_EXE_shoal"),
                "--json",
                "pr",
                "clear"
            ]),
            serde_json::json!({"registered": false})
        );
        fixture.restart();
        assert!(fixture.ok(&["inspect", name])["pr_cleanup"].is_null());
        assert_eq!(
            fixture.ok(&["pr", "clear", name]),
            serde_json::json!({"registered": false})
        );
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    }
}

#[test]
fn invalid_persisted_pr_registration_retains_workspace_and_can_be_cleared() {
    let mut fixture = Fixture::with_config(Some("[auto_cleanup]\nenabled=false\n"));
    let workspace = fixture.add("invalid");
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    for record in [
        serde_json::json!({"url": null, "head": null, "error": null}),
        serde_json::json!({"url": "https://forge.example/team/repo/pulls/7", "head": "abc123", "error": null}),
    ] {
        db.execute(
            "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
            rusqlite::params![workspace["id"].as_str().unwrap(), record.to_string()],
        )
        .unwrap();
        fixture.restart();
        let output = fixture.run(&["inspect", "invalid"]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("expected exactly one of url or head")
        );
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
        fixture.ok(&["pr", "clear", "invalid"]);
        assert!(fixture.ok(&["inspect", "invalid"])["pr_cleanup"].is_null());
    }
}
