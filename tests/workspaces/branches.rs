//! Branches: diffs, syncing the default branch, and local merges and
//! unmerges.
use super::*;

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
        let review = fixture.run(&["review", "run", "changes", "--manual", "--", "{diff_base}"]);
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
    install_test_script(&external, "#!/bin/sh\nprintf 'configured-diff\\n'\n");
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
            .run(&["review", "run", "changes", "--manual"])
            .status
            .success()
    );
    assert_eq!(
        fixture
            .run(&["plain", "changes", "--", "{diff_base}"])
            .stdout,
        b"{diff_base}"
    );
    fixture.ok(&["rm", "--wait", "changes", "--yes"]);
}

#[test]
fn sync_fetches_the_remote_and_fast_forwards_only_the_default_branch() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    let head = git(path, &["rev-parse", "HEAD"]);
    let author = upstream_remote(&fixture);
    let upstream = git(&author, &["rev-parse", "HEAD"]);
    git(&author, &["switch", "-c", "feature/api"]);
    commit_file(&author, "api", "feature\n");
    git(&author, &["push", "origin", "feature/api"]);
    let feature = git(&author, &["rev-parse", "HEAD"]);
    let binary = support::shoal_binary();
    // A scoped agent syncs its own repository without moving its branch.
    let output = fixture.run(&[
        "run",
        "--workspace",
        "worker",
        "--",
        binary,
        "--json",
        "repo",
        "sync",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    // The daemon's own sync may have advanced main first; either way it ends
    // at the upstream commit.
    assert_eq!(result["remote"], "origin");
    assert_eq!(result["default_branch"]["commit"], upstream.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), upstream);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "origin/feature/api"]),
        feature
    );
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&fixture.repo, &["for-each-ref", "refs/shoal/"]), "");
    // A diverged default branch is reported, not rewritten.
    commit_file(&fixture.repo, "local", "local\n");
    let local = git(&fixture.repo, &["rev-parse", "main"]);
    git(&author, &["switch", "main"]);
    commit_file(&author, "remote", "remote\n");
    git(&author, &["push", "origin", "main"]);
    let output = fixture.run(&["repo", "sync", fixture.repo.to_str().unwrap()]);
    assert!(!output.status.success());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), local);
    // Scoped callers cannot sync another repository.
    let other = fixture.root.path().join("other");
    git(fixture.root.path(), &["init", "-b", "main", "other"]);
    commit_file(&other, "file", "other\n");
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let output = fixture.run(&[
        "run",
        "--workspace",
        "worker",
        "--",
        binary,
        "repo",
        "sync",
        other.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
}

#[test]
fn merge_into_main_without_a_remote_needs_approval_in_scoped_processes() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    commit_file(path, "merged", "from the workspace\n");
    let expected = git(path, &["rev-parse", "HEAD"]);
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    let binary = support::shoal_binary();
    let pending = fixture.run(&[
        "run",
        "--workspace",
        "worker",
        "--",
        binary,
        "--json",
        "merge",
    ]);
    assert_eq!(pending.status.code(), Some(2));
    let pending: serde_json::Value = serde_json::from_slice(&pending.stdout).unwrap();
    assert_eq!(pending["code"], "approval_pending");
    assert_eq!(pending["request"]["target"], "merge");
    assert_eq!(pending["request"]["specification"]["head"], expected.trim());
    let refusal = fixture.root.path().join("merge-result.json");
    let denied = fixture.run(&[
        "run",
        "--workspace",
        "worker",
        "--",
        binary,
        "internal",
        "merge",
        "--result",
        refusal.to_str().unwrap(),
        "{}",
    ]);
    assert!(!denied.status.success());
    let repo = fixture.ok(&["repo", "ls"])[0]["id"].clone();
    fixture.ok(&[
        "config",
        "set",
        "merge.push",
        "true",
        "--repo",
        repo.as_str().unwrap(),
    ]);
    let unpushable = fixture.run(&["merge", "worker"]);
    assert!(!unpushable.status.success());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    fixture.ok(&[
        "approvals",
        "--approve",
        pending["request"]["number"].as_str().unwrap(),
    ]);
    let merged = fixture.run(&[
        "run",
        "--workspace",
        "worker",
        "--",
        binary,
        "--json",
        "merge",
        "--no-push",
    ]);
    assert!(merged.status.success(), "{merged:?}");
    let result: serde_json::Value = serde_json::from_slice(&merged.stdout).unwrap();
    assert_eq!(result["updated"], true);
    assert_eq!(result["fast_forward"], true);
    assert_eq!(result["default_branch"], "main");
    assert_eq!(result["previous_commit"], before.trim());
    assert_eq!(result["commit"], expected.trim());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), expected);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("merged")).unwrap(),
        "from the workspace\n"
    );
    assert_eq!(
        fixture.ok(&["merge", "--no-push", "worker"])["updated"],
        false
    );
    // Merged work is on the default branch, so its branch goes.
    assert_eq!(
        fixture.ok(&["rm", "--wait", "worker"])["branch_deleted"],
        true
    );
}

#[test]
fn unmerge_moves_main_back_after_an_agents_request_is_approved() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    commit_file(path, "merged", "from the workspace\n");
    let head = git(path, &["rev-parse", "HEAD"]);
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    fixture.ok(&["merge", "--no-push", "worker"]);
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), head);
    let binary = support::shoal_binary();
    let as_agent = |args: &[&str]| {
        let mut command = vec!["run", "--workspace", "worker", "--", binary];
        command.extend_from_slice(args);
        fixture.run(&command)
    };
    let pending = as_agent(&["--json", "unmerge"]);
    assert_eq!(pending.status.code(), Some(2));
    let pending: serde_json::Value = serde_json::from_slice(&pending.stdout).unwrap();
    assert_eq!(pending["code"], "approval_pending");
    assert_eq!(pending["request"]["target"], "unmerge");
    assert_eq!(pending["request"]["specification"]["result"], head.trim());
    let refusal = fixture.root.path().join("unmerge-result.json");
    let forged = as_agent(&[
        "internal",
        "unmerge",
        "--result",
        refusal.to_str().unwrap(),
        "{}",
    ]);
    assert!(!forged.status.success());
    let refusal = fs::read_to_string(&refusal).unwrap();
    assert!(refusal.contains("only an authorized unmerge"), "{refusal}");
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), head);

    fixture.ok(&[
        "approvals",
        "--approve",
        pending["request"]["number"].as_str().unwrap(),
    ]);
    let unmerged = as_agent(&["--json", "unmerge"]);
    assert!(unmerged.status.success(), "{unmerged:?}");
    let unmerged: serde_json::Value = serde_json::from_slice(&unmerged.stdout).unwrap();
    assert_eq!(unmerged["commit"], before.trim());
    assert_eq!(unmerged["replayed"], 0);
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    assert!(!fixture.repo.join("merged").exists());
    assert_eq!(git(&fixture.repo, &["rev-parse", "worker"]), head);
    // The record went with the merge, so nothing is left to undo.
    let again = fixture.run(&["unmerge", "worker"]);
    assert!(!again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("no local merge of worker"),
        "{again:?}"
    );

    // A workspace cleanup removed comes back before its merge is undone.
    fixture.ok(&["merge", "--no-push", "worker"]);
    fixture.ok(&["rm", "--wait", "worker", "--yes"]);
    let removed = fixture.run(&["unmerge", "worker"]);
    assert!(!removed.status.success());
    assert!(
        String::from_utf8_lossy(&removed.stderr).contains("restore it with shoal restore worker"),
        "{removed:?}"
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), head);
    fixture.ok(&["restore", "worker"]);
    fixture.ok(&["unmerge", "worker"]);
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
}

#[test]
fn a_merge_that_pushes_asks_first_unless_confirmed() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["config", "user.name", "Test"]);
    git(
        &fixture.repo,
        &["config", "user.email", "test@example.invalid"],
    );
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    upstream_remote(&fixture);
    let origin = fixture.root.path().join("origin.git");
    let pushed = || git(&origin, &["rev-parse", "main"]);
    let main = || git(&fixture.repo, &["rev-parse", "main"]);
    commit_file(path, "first", "one\n");
    let (before, upstream) = (main(), pushed());
    // Without a terminal, a merge that pushes is refused before anything changes.
    let refused = fixture.run(&["merge", "--push", "worker"]);
    assert!(!refused.status.success());
    assert_eq!((main(), pushed()), (before.clone(), upstream.clone()));
    let (declined, transcript) = fixture.interactive(&["merge", "--push", "worker"], "n\n");
    assert!(!declined.status.success(), "{transcript}");
    assert_eq!((main(), pushed()), (before, upstream));
    let (accepted, transcript) = fixture.interactive(&["merge", "--push", "worker"], "y\n");
    assert!(accepted.status.success(), "{transcript}");
    assert_eq!(pushed(), main());
    // A merge that stays local never asks.
    commit_file(path, "second", "two\n");
    assert_eq!(fixture.ok(&["merge", "worker"])["updated"], true);
    // --yes, or [merge] confirm = false, skips the question.
    commit_file(path, "third", "three\n");
    let confirmed = fixture.ok(&["merge", "--push", "--yes", "worker"]);
    assert_eq!(confirmed["pushed_to"], "origin");
    let repo = fixture.ok(&["repo", "ls"])[0]["id"].clone();
    let repo = repo.as_str().unwrap();
    fixture.ok(&["config", "set", "merge.confirm", "false", "--repo", repo]);
    commit_file(path, "fourth", "four\n");
    assert_eq!(
        fixture.ok(&["merge", "--push", "worker"])["pushed_to"],
        "origin"
    );
    assert_eq!(pushed(), main());
}

#[test]
fn merge_interruptions_stop_merge_drivers_and_restore_the_default_checkout() {
    for interruption in ["stop", "daemon", "interrupt"] {
        let mut fixture = Fixture::new();
        git(&fixture.repo, &["config", "user.name", "Test"]);
        git(
            &fixture.repo,
            &["config", "user.email", "test@example.invalid"],
        );
        fs::write(fixture.repo.join(".gitattributes"), "tracked merge=slow\n").unwrap();
        commit_file(&fixture.repo, ".gitattributes", "tracked merge=slow\n");
        let worker = fixture.add("worker");
        let path = Path::new(worker["path"].as_str().unwrap());
        commit_file(path, "tracked", "worker\n");
        commit_file(path, "a-added", "new file\n");
        commit_file(&fixture.repo, "tracked", "main\n");
        let before = git(&fixture.repo, &["rev-parse", "HEAD"]);
        fs::write(
            fixture.repo.join(".git/info/exclude"),
            ".merge_file_ABC123\n",
        )
        .unwrap();
        fs::write(fixture.repo.join(".merge_file_ABC123"), "preexisting\n").unwrap();
        // Only the default checkout has the driver: the daemon's cleanup
        // sweep makes trial merges in the workspace, where the driver's
        // temporary inputs would leave the workspace unclean to merge.
        git(
            &fixture.repo,
            &["config", "extensions.worktreeConfig", "true"],
        );
        git(
            &fixture.repo,
            &[
                "config",
                "--worktree",
                "merge.slow.driver",
                "echo $$ > .git/merge-driver.pid.tmp; mv .git/merge-driver.pid.tmp .git/merge-driver.pid; exec sleep 60",
            ],
        );
        let stderr_path = fixture.root.path().join("merge.stderr");
        let mut merge = fixture
            .command()
            .args(["merge", "worker"])
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap();
        wait_until("merge driver to start", || {
            if fixture.repo.join(".git/merge-driver.pid").exists() {
                return true;
            }
            if let Some(status) = merge.try_wait().unwrap() {
                panic!(
                    "merge exited before merge driver with {status}: {}",
                    fs::read_to_string(&stderr_path).unwrap()
                );
            }
            false
        });
        let pid: i32 = fs::read_to_string(fixture.repo.join(".git/merge-driver.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let during = fixture.ok(&["status", "worker"]);
        assert_eq!(during["executions"].as_array().unwrap().len(), 1);
        assert!(during["executions"][0]["child"].is_object());
        // The daemon starts the merge, whose wrapper restores the checkout
        // before it exits; a killed daemon leaves it to do that after the CLI.
        let wrapper = during["executions"][0]["wrapper"]["pid"].as_u64().unwrap() as u32;
        if interruption == "daemon" {
            fixture.daemon.kill();
        } else if interruption == "interrupt" {
            assert_eq!(unsafe { libc::kill(merge.id() as i32, libc::SIGINT) }, 0);
        } else {
            fixture.ok(&["stop", "worker"]);
        }
        let status = wait_for("merge to fail", || merge.try_wait().unwrap());
        assert!(!status.success());
        wait_until("merge wrapper to stop", || !process_running(wrapper));
        wait_until("merge driver to stop", || !process_running(pid as u32));
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
