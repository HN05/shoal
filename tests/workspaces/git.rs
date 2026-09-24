use crate::support::{Fixture, git, upstream_remote};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn diff_excludes_new_main_commits_before_and_after_rebase_and_uses_git_configuration() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nreview = ['printf', '%s\\n', '{diff_base}..HEAD']\nplain = ['printf', '%s', '{args}']\n",
    ));
    let workspace = fixture.add("changes");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let commit = |path: &Path| {
        git(path, &["add", "."]);
        git(
            path,
            &[
                "-c",
                "user.name=Shoal Test",
                "-c",
                "user.email=shoal@example.invalid",
                "commit",
                "-m",
                "change",
            ],
        );
    };
    fs::write(path.join("tracked"), "workspace change\n").unwrap();
    commit(path);
    fs::write(fixture.repo.join("main-only"), "upstream-only content\n").unwrap();
    commit(&fixture.repo);
    for rebase in [false, true] {
        if rebase {
            git(
                path,
                &[
                    "-c",
                    "user.name=Shoal Test",
                    "-c",
                    "user.email=shoal@example.invalid",
                    "rebase",
                    "main",
                ],
            );
            fs::write(path.join("staged"), "staged workspace content\n").unwrap();
            git(path, &["add", "staged"]);
            fs::write(path.join("tracked"), "unstaged workspace content\n").unwrap();
        }
        let base = git(path, &["merge-base", "main", "HEAD"]);
        let review = fixture.run(&["review", "changes", "--manual", "--", "{diff_base}"]);
        assert!(
            review.status.success(),
            "{}",
            String::from_utf8_lossy(&review.stderr)
        );
        assert_eq!(
            String::from_utf8(review.stdout).unwrap(),
            format!("{}..HEAD\n{{diff_base}}\n", base.trim())
        );
        let output = fixture
            .command()
            .current_dir(path)
            .args(["diff"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let diff = String::from_utf8(output.stdout).unwrap();
        assert!(diff.contains("workspace"));
        assert!(!diff.contains("main-only") && !diff.contains("upstream-only"));
        if rebase {
            assert!(
                diff.contains("staged workspace content")
                    && diff.contains("unstaged workspace content")
            );
        }
    }
    let external = fixture.root.path().join("external-diff");
    fs::write(&external, "#!/bin/sh\nprintf 'configured-diff\\n'\n").unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        path,
        &["config", "diff.external", external.to_str().unwrap()],
    );
    let output = fixture.run(&["diff", "changes"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("configured-diff"));
    // A missing base blocks review, but is irrelevant to commands without the placeholder.
    git(&fixture.repo, &["branch", "-m", "renamed-main"]);
    assert!(
        !fixture
            .run(&["review", "changes", "--manual"])
            .status
            .success()
    );
    assert_eq!(
        fixture
            .run(&["plain", "changes", "--", "{diff_base}"])
            .stdout,
        b"{diff_base}"
    );
    fixture.ok(&["rm", "changes", "--yes", "--delete-branch"]);
}

fn merge_commit(path: &Path, file: &str, contents: &str) {
    fs::write(path.join(file), contents).unwrap();
    git(path, &["add", file]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            file,
        ],
    );
}

#[test]
fn merge_scoped_local_branch_only_changes_own_workspace() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let other = fixture.add("other");
    let worker_path = Path::new(worker["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    merge_commit(other_path, "incoming", "from another workspace\n");
    let expected = git(other_path, &["rev-parse", "HEAD"]);
    let main = git(&fixture.repo, &["rev-parse", "main"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    let denied = fixture.run(&["exec", "worker", "--", binary, "merge", "other", "other"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("cannot access another worktree"));
    let output = fixture.run(&["exec", "worker", "--", binary, "--json", "merge", "other"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["source_commit"], expected.trim());
    assert_eq!(result["success"], true);
    assert_eq!(git(worker_path, &["rev-parse", "HEAD"]), expected);
    assert_eq!(git(other_path, &["rev-parse", "HEAD"]), expected);
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), main);
    assert_eq!(
        git(worker_path, &["symbolic-ref", "--short", "HEAD"]),
        "worker\n"
    );
    assert!(
        fixture.ok(&["inspect", "worker"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn merge_refreshes_local_source_from_upstream_unless_local_or_owned_by_a_workspace() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    let stale = git(&fixture.repo, &["rev-parse", "main"]);
    let author = upstream_remote(&fixture);
    let upstream = git(&author, &["rev-parse", "HEAD"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    // A scoped agent merging main refreshes and uses the upstream state.
    let output = fixture.run(&["exec", "worker", "--", binary, "--json", "merge", "main"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["success"], true);
    assert_eq!(result["source_refresh"]["updated"], true);
    assert_eq!(result["source_refresh"]["previous_commit"], stale.trim());
    assert_eq!(result["source_commit"], upstream.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    assert_eq!(git(path, &["rev-parse", "HEAD"]), upstream);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("upstream")).unwrap(),
        "from remote\n"
    );
    // --local merges the local branch as it is and leaves upstream alone.
    merge_commit(&author, "upstream", "second\n");
    git(&author, &["push", "origin", "main"]);
    let result = fixture.ok(&["merge", "main", "worker", "--local"]);
    assert!(result["source_refresh"].is_null());
    assert_eq!(result["source_commit"], upstream.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    // A dirty main checkout blocks the refresh instead of merging stale work.
    fs::write(fixture.repo.join("scratch"), "dirty\n").unwrap();
    let output = fixture.run(&["merge", "main", "worker"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--local"), "{stderr}");
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    assert_eq!(git(path, &["rev-parse", "HEAD"]), upstream);
    fs::remove_file(fixture.repo.join("scratch")).unwrap();
    // Another workspace's branch is merged as it is, even with an upstream.
    let other = fixture.add("other");
    let other_path = Path::new(other["path"].as_str().unwrap());
    git(
        &fixture.repo,
        &["branch", "--set-upstream-to=origin/main", "other"],
    );
    merge_commit(other_path, "theirs", "other work\n");
    let theirs = git(other_path, &["rev-parse", "HEAD"]);
    let result = fixture.ok(&["merge", "other", "worker"]);
    assert_eq!(result["source_commit"], theirs.trim());
    assert!(
        result["source_refresh"]["skipped"]
            .as_str()
            .unwrap()
            .contains("workspace other")
    );
    assert_eq!(git(other_path, &["rev-parse", "HEAD"]), theirs);
    assert_eq!(git(&fixture.repo, &["for-each-ref", "refs/shoal/"]), "");
}

#[test]
fn merge_fetches_remote_only_branch_and_refreshes_qualified_sources() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    let author = upstream_remote(&fixture);
    git(&fixture.repo, &["remote", "rename", "origin", "source"]);
    git(&author, &["switch", "-c", "feature/remote-only"]);
    merge_commit(&author, "remote-only", "first\n");
    git(&author, &["push", "origin", "feature/remote-only"]);
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    // This branch has never been fetched into a local or remote-tracking ref.
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "for-each-ref",
                "refs/remotes/source/feature/remote-only",
                "refs/heads/feature/remote-only"
            ]
        ),
        ""
    );
    let fetch_head = fixture.repo.join(".git/FETCH_HEAD");
    fs::write(&fetch_head, "unrelated fetch sentinel\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_shoal");
    let output = fixture.run(&[
        "exec",
        "worker",
        "--",
        binary,
        "--json",
        "merge",
        "feature/remote-only",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git(path, &["rev-parse", "HEAD"]),
        git(&author, &["rev-parse", "HEAD"])
    );
    merge_commit(&author, "remote-only", "latest\n");
    git(&author, &["push", "origin", "feature/remote-only"]);
    let result = fixture.ok(&["merge", "source/feature/remote-only", "worker"]);
    assert_eq!(result["success"], true);
    assert_eq!(
        fs::read_to_string(path.join("remote-only")).unwrap(),
        "latest\n"
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    assert_eq!(
        fs::read_to_string(fetch_head).unwrap(),
        "unrelated fetch sentinel\n"
    );
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "for-each-ref",
                "refs/shoal/merge/",
                "refs/heads/feature/remote-only",
                "refs/remotes/source/feature/remote-only"
            ]
        ),
        ""
    );
    // A deleted remote branch must fail even if a stale tracking ref remains.
    git(
        &fixture.repo,
        &[
            "update-ref",
            "refs/remotes/source/feature/remote-only",
            "worker",
        ],
    );
    git(
        &author,
        &["push", "origin", "--delete", "feature/remote-only"],
    );
    assert!(
        !fixture
            .run(&["merge", "source/feature/remote-only", "worker"])
            .status
            .success()
    );
}

#[test]
fn merge_remote_ambiguity_local_precedence_and_explicit_remote() {
    let fixture = Fixture::new();
    fixture.add("worker");
    let author = upstream_remote(&fixture);
    git(&author, &["switch", "-c", "topic"]);
    git(&author, &["push", "origin", "topic"]);
    let remote = fixture.root.path().join("origin.git");
    git(
        &fixture.repo,
        &["remote", "add", "second", remote.to_str().unwrap()],
    );
    let before = git(&fixture.repo, &["rev-parse", "worker"]);
    let output = fixture.run(&["merge", "topic", "worker"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("multiple remotes"));
    assert_eq!(git(&fixture.repo, &["rev-parse", "worker"]), before);
    git(&fixture.repo, &["branch", "topic", "main"]);
    assert_eq!(
        fixture.ok(&["merge", "topic", "worker"])["commit"],
        before.trim()
    );
    let result = fixture.ok(&["merge", "topic", "worker", "--remote", "second"]);
    assert_eq!(
        result["source_commit"],
        git(&author, &["rev-parse", "HEAD"]).trim()
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "topic"]), before);
    assert!(
        !fixture
            .run(&["merge", "topic", "worker", "--remote", "missing"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["merge", "missing-branch", "worker"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["merge", "topic:worker", "worker"])
            .status
            .success()
    );
    assert_eq!(
        git(&fixture.repo, &["for-each-ref", "refs/shoal/merge/"]),
        ""
    );
}

#[test]
fn merge_preserves_conflicts_and_refuses_changed_destination_branch() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let other = fixture.add("other");
    let path = Path::new(worker["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    git(&fixture.repo, &["config", "user.name", "Test"]);
    git(
        &fixture.repo,
        &["config", "user.email", "test@example.invalid"],
    );
    merge_commit(path, "tracked", "ours\n");
    merge_commit(other_path, "tracked", "theirs\n");
    let ours = git(path, &["rev-parse", "HEAD"]);
    let theirs = git(other_path, &["rev-parse", "HEAD"]);
    let output = fixture.run(&["--json", "merge", "other", "worker"]);
    assert_eq!(output.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["success"], false);
    assert!(result["stdout"].as_str().unwrap().contains("CONFLICT"));
    assert_eq!(git(path, &["rev-parse", "HEAD"]), ours);
    assert_eq!(git(path, &["rev-parse", "MERGE_HEAD"]), theirs);
    assert!(
        fs::read_to_string(path.join("tracked"))
            .unwrap()
            .contains("<<<<<<<")
    );
    git(path, &["merge", "--abort"]);
    git(path, &["switch", "-c", "unowned"]);
    let output = fixture.run(&["merge", "other", "worker"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("own recorded branch"));
    assert_eq!(git(path, &["rev-parse", "HEAD"]), ours);
    git(path, &["switch", "--detach"]);
    assert!(!fixture.run(&["merge", "other", "worker"]).status.success());
}

#[test]
fn merge_creates_merge_commit_and_preserves_uncommitted_edits() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let other = fixture.add("other");
    let path = Path::new(worker["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    git(&fixture.repo, &["config", "user.name", "Test"]);
    git(
        &fixture.repo,
        &["config", "user.email", "test@example.invalid"],
    );
    merge_commit(path, "ours", "ours\n");
    merge_commit(other_path, "tracked", "theirs\n");
    fs::write(path.join("tracked"), "uncommitted\n").unwrap();
    let before = git(path, &["rev-parse", "HEAD"]);
    assert!(!fixture.run(&["merge", "other", "worker"]).status.success());
    assert_eq!(
        fs::read_to_string(path.join("tracked")).unwrap(),
        "uncommitted\n"
    );
    assert_eq!(git(path, &["rev-parse", "HEAD"]), before);
    git(path, &["restore", "tracked"]);
    let result = fixture.ok(&["merge", "other", "worker"]);
    assert_eq!(result["success"], true);
    assert_eq!(git(path, &["rev-parse", "HEAD^1"]), before);
    assert_eq!(
        git(path, &["rev-parse", "HEAD^2"]),
        git(other_path, &["rev-parse", "HEAD"])
    );
    assert_eq!(fs::read_to_string(path.join("ours")).unwrap(), "ours\n");
}

#[test]
fn land_merges_into_main_without_a_remote_and_is_denied_to_scoped_processes() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    merge_commit(path, "landed", "from the workspace\n");
    let expected = git(path, &["rev-parse", "HEAD"]);
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    let binary = env!("CARGO_BIN_EXE_shoal");
    let denied = fixture.run(&["exec", "worker", "--", binary, "--json", "land"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("cannot land"));
    let denied = fixture.run(&["exec", "worker", "--", binary, "land-internal", "{}"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("only an authorized landing"));
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    let result = fixture.ok(&["land", "worker"]);
    assert_eq!(result["updated"], true);
    assert_eq!(result["fast_forward"], true);
    assert_eq!(result["default_branch"], "main");
    assert_eq!(result["previous_commit"], before.trim());
    assert_eq!(result["commit"], expected.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), expected);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("landed")).unwrap(),
        "from the workspace\n"
    );
    assert_eq!(fixture.ok(&["land", "worker"])["updated"], false);
    // Landed work needs no branch choice.
    assert_eq!(fixture.ok(&["rm", "worker"])["branch_deleted"], true);
}

#[test]
fn land_interruptions_stop_merge_drivers_and_restore_the_default_checkout() {
    for interruption in ["stop", "daemon", "interrupt"] {
        let mut fixture = Fixture::new();
        git(&fixture.repo, &["config", "user.name", "Test"]);
        git(
            &fixture.repo,
            &["config", "user.email", "test@example.invalid"],
        );
        fs::write(fixture.repo.join(".gitattributes"), "tracked merge=slow\n").unwrap();
        merge_commit(&fixture.repo, ".gitattributes", "tracked merge=slow\n");
        let worker = fixture.add("worker");
        let path = Path::new(worker["path"].as_str().unwrap());
        merge_commit(path, "tracked", "worker\n");
        merge_commit(path, "a-added", "new file\n");
        merge_commit(&fixture.repo, "tracked", "main\n");
        let before = git(&fixture.repo, &["rev-parse", "HEAD"]);
        fs::write(
            fixture.repo.join(".git/info/exclude"),
            ".merge_file_ABC123\n",
        )
        .unwrap();
        fs::write(fixture.repo.join(".merge_file_ABC123"), "preexisting\n").unwrap();
        git(
            &fixture.repo,
            &[
                "config",
                "merge.slow.driver",
                "echo $$ > .git/land-driver.pid; exec sleep 60",
            ],
        );
        let stderr_path = fixture.root.path().join("land.stderr");
        let mut land = fixture
            .command()
            .args(["land", "worker"])
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fixture.repo.join(".git/land-driver.pid").exists() {
            assert!(
                land.try_wait().unwrap().is_none(),
                "land exited before merge driver"
            );
            assert!(Instant::now() < deadline, "merge driver did not start");
            thread::sleep(Duration::from_millis(20));
        }
        let pid: i32 = fs::read_to_string(fixture.repo.join(".git/land-driver.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let during = fixture.ok(&["inspect", "worker"]);
        assert_eq!(during["executions"].as_array().unwrap().len(), 1);
        assert!(during["executions"][0]["child"].is_object());
        if interruption == "daemon" {
            fixture.daemon.kill().unwrap();
            fixture.daemon.wait().unwrap();
        } else if interruption == "interrupt" {
            assert_eq!(unsafe { libc::kill(land.id() as i32, libc::SIGINT) }, 0);
        } else {
            fixture.ok(&["stop", "worker"]);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = land.try_wait().unwrap() {
                assert!(!status.success());
                break;
            }
            assert!(Instant::now() < deadline, "landing wrapper did not stop");
            thread::sleep(Duration::from_millis(20));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            let status = String::from_utf8_lossy(&output.stdout);
            if status.trim().is_empty() || status.trim().starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "merge driver survived landing stop"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            git(&fixture.repo, &["status", "--porcelain"]),
            "",
            "{interruption}: {}",
            fs::read_to_string(&stderr_path).unwrap()
        );
        assert_eq!(
            git(&fixture.repo, &["write-tree"]),
            git(&fixture.repo, &["rev-parse", "HEAD^{tree}"])
        );
        assert!(!fixture.repo.join(".git/index.lock").exists());
        assert!(!fixture.repo.join(".git/MERGE_HEAD").exists());
        assert_eq!(
            fs::read_to_string(fixture.repo.join(".merge_file_ABC123")).unwrap(),
            "preexisting\n"
        );
        assert!(path.exists());
    }
}
