//! Workspace names: renaming a workspace, and the names and branches a new
//! one gets.
use super::forge::github_issue;
use super::recovery::{OWNER_MARKER, recovery_report, repaired_workspaces};
use super::*;

#[test]
fn workspace_rename_cli_infers_context_and_enforces_execution_scope() {
    let fixture = Fixture::new();
    let workspace = fixture.add("original");
    fixture.add("other");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let binary = support::shoal_binary();
    let denied = fixture.run(&[
        "run",
        "--workspace",
        "original",
        "--",
        binary,
        "--json",
        "branch",
        "rename",
        "other",
        "stolen",
    ]);
    assert!(!denied.status.success());
    let error: Value = serde_json::from_slice(&denied.stderr).unwrap();
    assert_eq!(error["error"]["code"], "scope_denied", "{error}");
    assert_eq!(
        fixture.ok(&["status", "other"])["workspace"]["branch"],
        "other"
    );
    let renamed = fixture.ok(&[
        "run",
        "--workspace",
        "original",
        "--",
        binary,
        "--json",
        "branch",
        "rename",
        "feature/topic",
    ]);
    assert_eq!(renamed["branch"], "feature/topic");
    assert_eq!(renamed["path"], workspace["path"]);
    let output = fixture
        .command()
        .current_dir(path)
        .args(["--json", "branch", "rename", "by-directory"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["name"],
        "by-directory"
    );
    let explicit = fixture.ok(&["branch", "rename", "by-directory", "explicit"]);
    assert_eq!(explicit["name"], "explicit");
    // JSON errors name the refusal, so programs need not read the message.
    for (args, code) in [
        (
            &["--json", "branch", "rename", "explicit", "other"][..],
            "name_taken",
        ),
        (&["--json", "status", "missing"], "not_found"),
    ] {
        let output = fixture.run(args);
        assert!(!output.status.success(), "{args:?}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], code, "{args:?}: {error}");
    }
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO executions(id,workspace_id,state) VALUES ('unknown',?1,'unknown')",
        [workspace["id"].as_str().unwrap()],
    )
    .unwrap();
    let denied = fixture.rename("explicit", "blocked");
    assert_eq!(denied["type"], "error");
    assert_eq!(
        fixture.ok(&["status", "explicit"])["workspace"]["branch"],
        "explicit"
    );
    db.execute("DELETE FROM executions WHERE id='unknown'", [])
        .unwrap();
    let output = fixture.run(&["run", "--workspace", "explicit", "--", "/usr/bin/env"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("SHOAL_WORKSPACE=explicit\n"));
}

#[test]
fn workspace_rename_preserves_work_ownership_and_diff_bases() {
    let mut fixture =
        Fixture::with_config(Some("[resources.lock]\n[cleanup.done]\nenabled = false\n"));
    let workspace = fixture.add("original");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let id = workspace["id"].as_str().unwrap();
    fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "dependent",
        "--base",
        "original",
    ]);
    fs::write(path.join("dirty"), "preserve\n").unwrap();
    git(path, &["config", "branch.original.remote", "."]);
    git(
        path,
        &["config", "branch.original.merge", "refs/heads/main"],
    );
    fixture.ok(&["acquire", "port", "web", "--workspace", "original"]);
    fixture.ok(&["acquire", "resource", "lock", "--workspace", "original"]);
    // A PR registration names no branch, so it stays through the rename.
    fixture.acknowledge("original");
    fixture.ok(&["done", "original"]);
    let before = fixture.ok(&["status", "original"]);
    assert!(before["pr_cleanup"]["head"].is_string(), "{before}");
    let response = fixture.rename("original", "feature/topic");
    assert_eq!(response["type"], "workspace", "{response}");
    let renamed = &response["data"];
    assert_eq!(renamed["name"], "feature-topic");
    assert_eq!(renamed["branch"], "feature/topic");
    assert_eq!(
        fixture.ok(&["status", "feature-topic"])["workspace"]["branch"],
        "feature/topic"
    );
    fs::write(path.join("after-rename"), "changed after rename\n").unwrap();
    git(path, &["add", "after-rename"]);
    let diff = fixture.run(&["diff", "feature-topic"]);
    assert!(diff.status.success());
    assert!(String::from_utf8_lossy(&diff.stdout).contains("changed after rename"));
    // The new name selects the workspace and its completion.
    fixture.ok(&["done", "--undo", "feature-topic"]);
    fixture.ok(&["done", "feature-topic"]);
    for field in [
        "id",
        "path",
        "git_dir",
        "git_dir_id",
        "base_ref",
        "base_commit",
    ] {
        assert_eq!(renamed[field], workspace[field]);
    }
    assert!(!fixture.run(&["status", "original"]).status.success());
    assert_eq!(
        git(path, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/feature/topic"
    );
    assert_eq!(
        git(path, &["rev-parse", "feature/topic@{upstream}"]),
        git(path, &["rev-parse", "main"])
    );
    assert!(
        git(path, &["reflog", "show", "feature/topic"]).contains("renamed refs/heads/original")
    );
    assert_eq!(
        fs::read_to_string(path.join("dirty")).unwrap(),
        "preserve\n"
    );
    assert_eq!(
        fixture.ok(&["status", "dependent"])["workspace"]["base_ref"],
        "refs/heads/feature/topic"
    );
    fixture.restart();
    let after = fixture.ok(&["status", id]);
    for field in ["ports", "resources"] {
        assert_eq!(after[field], before[field]);
    }
    assert_eq!(after["completion"]["head"], before["completion"]["head"]);
    assert_eq!(after["pr_cleanup"]["head"], before["pr_cleanup"]["head"]);
    assert_eq!(after["workspace"], *renamed);
}

#[test]
fn workspace_rename_issue_reopens_the_associated_workspace() {
    let fixture = Fixture::with_config(Some("[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n"));
    fixture.add_github_origin();
    fixture.script_github(&github_issue(34, "Fix API timeout", ""));
    let repo = fixture.repo.to_str().unwrap();
    let issue = ["start", "34", "--repo", repo, "--here", "--agent", "pi"];
    let created = fixture.ok(&[&issue[..], &["--base", "HEAD"]].concat());
    let renamed = fixture.ok(&["branch", "rename", "34-fix-api-timeout", "renamed-issue"]);
    let reopened = fixture.ok(&issue);
    assert_eq!(reopened["id"], created["id"]);
    assert_eq!(reopened["path"], created["path"]);
    assert_eq!(reopened["branch"], renamed["branch"]);
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

#[test]
fn workspace_rename_refuses_collisions_and_recovers_git_failure() {
    let fixture = Fixture::new();
    let workspace = fixture.add("original");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fixture.add("other-name");
    git(&fixture.repo, &["branch", "taken"]);
    for branch in [
        "other/name",
        "taken",
        "bad name",
        "@{-1}",
        "HEAD",
        "@",
        "0123456789abcdef0123456789abcdef01234567",
        "taken/child",
    ] {
        let response = fixture.rename("original", branch);
        assert_eq!(response["type"], "error", "{branch}: {response}");
        assert_eq!(fixture.ok(&["status", "original"])["workspace"], workspace);
        assert_eq!(
            git(path, &["symbolic-ref", "HEAD"]).trim(),
            "refs/heads/original"
        );
    }
    git(path, &["switch", "--detach"]);
    assert_eq!(fixture.rename("original", "new-name")["type"], "error");
    git(path, &["switch", "original"]);
    // A different literal branch may have the same normalized workspace name.
    assert_eq!(fixture.rename("original", "original")["type"], "workspace");
    assert_eq!(fixture.rename("original", "new-name")["type"], "workspace");
    assert_eq!(fixture.rename("new-name", "new/name")["type"], "workspace");
    fixture.ok(&["rm", "--wait", "new-name", "--yes"]);
    assert!(!path.exists());
    assert!(
        git(
            &fixture.repo,
            &["for-each-ref", "--format=%(refname)", "refs/heads/new"]
        )
        .is_empty()
    );
    let reused = fixture.add("original");
    assert_eq!(reused["branch"], "original");
    assert_ne!(reused["id"], workspace["id"]);
}

#[test]
fn interrupted_workspace_rename_requires_explicit_repair() {
    for branch in ["original", "new/topic", "unrelated"] {
        let mut fixture = Fixture::with_config(Some("[resources.lock]\n"));
        let workspace = fixture.add("original");
        let path = Path::new(workspace["path"].as_str().unwrap());
        let id = workspace["id"].as_str().unwrap();
        fs::write(path.join("dirty"), "preserve\n").unwrap();
        let port = fixture.ok(&["acquire", "port", "web", "--workspace", "original"]);
        let lease = fixture.ok(&["acquire", "resource", "lock", "--workspace", "original"]);
        let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
        db.execute("INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,'new-topic','new/topic')", [id]).unwrap();
        db.execute(
            "UPDATE workspaces SET state='reconciling' WHERE id=?1",
            [id],
        )
        .unwrap();
        if branch != "original" {
            git(path, &["branch", "-m", branch]);
        }
        if branch == "new/topic" {
            fs::remove_file(Path::new(workspace["git_dir"].as_str().unwrap()).join(OWNER_MARKER))
                .unwrap();
            db.execute("UPDATE workspaces SET git_dir_id='birth:1:2:3'", [])
                .unwrap();
        }
        fixture.restart();
        assert_eq!(fixture.ok(&["status", id])["workspace"]["state"], "failed");
        let rejected = fixture.run(&["start", fixture.repo.to_str().unwrap(), "new-topic"]);
        assert!(!rejected.status.success());
        if branch == "unrelated" {
            recovery_report(&fixture, &[id, "--repair"]);
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM workspace_renames", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            git(path, &["branch", "-m", "original"]);
        }
        let repair_args = if branch == "new/topic" {
            vec![id, "--repair", "--reclaim"]
        } else {
            vec![id, "--repair"]
        };
        repaired_workspaces(&fixture, &repair_args);
        let inspection = fixture.ok(&["status", id]);
        assert_eq!(inspection["workspace"]["state"], "ready");
        assert_eq!(
            inspection["workspace"]["branch"],
            if branch == "new/topic" {
                "new/topic"
            } else {
                "original"
            }
        );
        assert_eq!(
            inspection["workspace"]["name"],
            if branch == "new/topic" {
                "new-topic"
            } else {
                "original"
            }
        );
        assert_eq!(inspection["workspace"]["path"], workspace["path"]);
        assert_eq!(inspection["ports"][0], port);
        assert_eq!(inspection["resources"][0], lease);
        assert_eq!(
            fs::read_to_string(path.join("dirty")).unwrap(),
            "preserve\n"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM workspace_renames", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn work_rename_finishes_an_interrupted_rename() {
    for (git_branch, requested) in [("original", "new/topic"), ("new/topic", "new/topic")] {
        let mut fixture = Fixture::new();
        let workspace = fixture.add("original");
        let path = Path::new(workspace["path"].as_str().unwrap());
        let id = workspace["id"].as_str().unwrap();
        let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
        db.execute("INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,'new-topic','new/topic')", [id]).unwrap();
        db.execute(
            "UPDATE workspaces SET state='reconciling' WHERE id=?1",
            [id],
        )
        .unwrap();
        if git_branch != "original" {
            git(path, &["branch", "-m", git_branch]);
        }
        db.execute(
            "INSERT INTO executions(id,workspace_id,state) VALUES ('leftover',?1,'unknown')",
            [id],
        )
        .unwrap();
        fixture.restart();
        assert_eq!(fixture.ok(&["status", id])["workspace"]["state"], "failed");
        // Another recorded execution keeps the rename unfinished.
        let refused = fixture.run(&["branch", "rename", id, requested]);
        assert!(!refused.status.success());
        assert_eq!(fixture.ok(&["status", id])["workspace"]["state"], "failed");
        db.execute("DELETE FROM executions WHERE id='leftover'", [])
            .unwrap();
        // Running the rename again finishes or cancels the interrupted one,
        // then renames to the branch asked for.
        let renamed = fixture.ok(&["branch", "rename", id, requested]);
        assert_eq!(renamed["branch"], "new/topic");
        assert_eq!(renamed["state"], "ready");
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM workspace_renames", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn deleted_worktree_with_interrupted_rename_can_be_removed() {
    let fixture = Fixture::with_config(Some("[resources.lock]\n"));
    let workspace = fixture.add("original");
    // A commit of its own keeps the branch from the kept-branch sweep.
    commit_file(
        Path::new(workspace["path"].as_str().unwrap()),
        "original",
        "work",
    );
    fixture.ok(&["acquire", "port", "web", "--workspace", "original"]);
    fixture.ok(&["acquire", "resource", "lock", "--workspace", "original"]);
    let id = workspace["id"].as_str().unwrap();
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,'new-topic','new/topic')",
        [id],
    )
    .unwrap();
    db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [id])
        .unwrap();
    fs::remove_dir_all(workspace["path"].as_str().unwrap()).unwrap();
    drop(db);
    let removed = fixture.ok(&["rm", "--wait", "original"]);
    assert_eq!(removed["branch_deleted"], false);
    assert!(!fixture.run(&["status", "original"]).status.success());
    assert!(
        fixture
            .ok(&["leases", "port", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .ok(&["leases", "resource", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!git(&fixture.repo, &["rev-parse", "refs/heads/original"]).is_empty());
}

#[test]
fn concurrent_adds_cannot_claim_the_same_name() {
    let fixture = Fixture::new();
    for (first_name, second_name, workspace_name) in [
        ("shared", "shared", "shared"),
        ("shared/topic", "shared-topic", "shared-topic"),
    ] {
        let first = fixture
            .command()
            .args(["start", fixture.repo.to_str().unwrap(), first_name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let second = fixture.run(&["start", fixture.repo.to_str().unwrap(), second_name]);
        let first = first.wait_with_output().unwrap();
        assert_ne!(first.status.success(), second.status.success());
        assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
        fixture.ok(&["rm", "--wait", workspace_name]);
    }
}

#[test]
fn branch_names_are_preserved_with_portable_workspace_names() {
    let fixture = Fixture::new();
    // Branches Git accepts but a shell or path would not take literally;
    // the daemon's branch tests cover the names Git reads as revisions.
    for (branch, name) in [
        ("release/v1.2", "release-v1-2"),
        ("feature/æøå-日本語", "feature--------"),
        ("refs/heads/topic", "refs-heads-topic"),
        ("topic/-leaf", "topic--leaf"),
        ("\u{2003}topic\u{2003}", "topic-"),
        (
            "topic/quote'\";$`(literal)&{ok}",
            "topic-quote------literal---ok-",
        ),
    ] {
        git(&fixture.repo, &["check-ref-format", "--branch", branch]);
        let workspace = fixture.add(branch);
        assert_eq!(workspace["branch"], branch);
        assert_eq!(workspace["name"], name, "{branch}");
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(path.file_name().unwrap(), name);
        assert_eq!(
            git(path, &["symbolic-ref", "HEAD"]),
            format!("refs/heads/{branch}\n")
        );
        assert_eq!(fixture.ok(&["rm", "--wait", name])["branch_deleted"], true);
    }
}

#[test]
fn invalid_branch_names_and_normalized_name_collisions_preserve_existing_work() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["switch", "-c", "previous"]);
    git(&fixture.repo, &["switch", "main"]);
    for branch in [
        "",
        "../escape",
        "/absolute",
        "trailing/",
        "double//slash",
        "bad..name",
        ".hidden",
        "x/.hidden",
        "bad.lock",
        "x/bad.lock/y",
        "bad.",
        "bad name",
        "bad\nname",
        "bad\\name",
        "bad~name",
        "bad^name",
        "bad:name",
        "bad?name",
        "bad*name",
        "bad[name",
        "-option",
        "@{1}",
        "@{-1}",
    ] {
        assert!(
            !fixture
                .run(&["start", fixture.repo.to_str().unwrap(), branch])
                .status
                .success(),
            "{branch}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
    git(&fixture.repo, &["branch", "-d", "previous"]);
    let existing = fixture.add("feature/topic");
    for branch in ["feature-topic", "feature.topic"] {
        let output = fixture.run(&["start", fixture.repo.to_str().unwrap(), branch]);
        assert!(!output.status.success());
    }
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([existing]));
    assert_eq!(
        git(
            &fixture.repo,
            &["for-each-ref", "--format=%(refname)", "refs/heads/"]
        ),
        "refs/heads/feature/topic\nrefs/heads/main\n"
    );
}
