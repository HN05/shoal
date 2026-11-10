//! Removing workspaces: confirmation, kept branches, holds, cleanup, and
//! restoring removed workspaces.
use super::chats::start_test_chat;
use super::forge::wait_removed;
use super::*;

#[test]
fn cleanup_removes_idle_candidates_on_request() {
    let fixture = Fixture::new();
    let clean = fixture.add("clean");
    let dirty = fixture.add("dirty");
    fs::write(
        Path::new(dirty["path"].as_str().unwrap()).join("work"),
        "keep",
    )
    .unwrap();
    let binary = support::shoal_binary();
    let denied = fixture.run(&["run", "--workspace", "dirty", "--", binary, "cleanup"]);
    assert!(!denied.status.success());
    // Chats are spaces too: one whose agent stopped goes, a running one stays.
    let (mut talking, _) = start_test_chat(&fixture, "talk");
    let (mut parked, _) = start_test_chat(&fixture, "parked");
    parked.kill().unwrap();
    parked.wait().unwrap();
    let preview = fixture.ok(&["cleanup", "--dry-run"]);
    assert_eq!(preview["removed"], serde_json::json!(["clean", "parked"]));
    assert_eq!(
        preview["kept"],
        serde_json::json!([
            {"space": "dirty", "reason": {"reason": "dirty"}},
            {"space": "talk", "reason": {"reason": "agent_running"}},
        ])
    );
    // Without a terminal, cleanup without a kind only lists.
    assert_eq!(fixture.ok(&["cleanup"])["dry_run"], true);
    assert!(Path::new(clean["path"].as_str().unwrap()).exists());
    // A named workspace that stays says why.
    let named = fixture.ok(&["cleanup", "spaces", "dirty"]);
    assert_eq!(named["removed"], serde_json::json!([]));
    assert_eq!(named["kept"][0]["space"], "dirty");
    assert!(
        !fixture
            .run(&["cleanup", "spaces", "missing"])
            .status
            .success()
    );
    let report = fixture.ok(&["cleanup", "spaces"]);
    assert_eq!(report["removed"], serde_json::json!(["clean", "parked"]));
    assert_eq!(report["retained"], serde_json::json!([]));
    assert_eq!(report["kept"], serde_json::json!([]));
    assert!(!Path::new(clean["path"].as_str().unwrap()).exists());
    let chats = fixture.ok(&["work", "--chats"]);
    assert_eq!(chats.as_array().unwrap().len(), 1, "{chats}");
    assert_eq!(chats[0]["name"], "talk", "{chats}");
    talking.kill().unwrap();
    talking.wait().unwrap();
    let names: Vec<_> = fixture
        .ok(&["work"])
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].clone())
        .collect();
    assert_eq!(names, ["dirty"]);
}

#[test]
fn restore_reopens_a_removed_workspace_with_its_unsaved_files() {
    let fixture = Fixture::new();
    let workspace = fixture.add("scratch");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(path.join("notes"), "draft\n").unwrap();
    fixture.ok(&["rm", "scratch", "--yes"]);
    assert!(!path.exists());
    let removed = fixture.ok(&["work", "restore"]);
    assert_eq!(removed[0]["name"], "scratch");
    assert_eq!(removed[0]["cause"], "manual");

    let restored = fixture.ok(&["work", "restore", "scratch"]);
    assert_eq!(restored["workspace"]["name"], "scratch");
    assert_eq!(restored["workspace"]["state"], "ready");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
    assert_eq!(fixture.ok(&["work", "restore"]), serde_json::json!([]));
    let missing = fixture.run(&["work", "restore", "missing"]);
    assert!(!missing.status.success());

    // The restored workspace keeps the removal's checkpoint, and returns to
    // any checkpoint after keeping the state it replaces.
    let checkpoints = fixture.ok(&["work", "restore", "scratch"]);
    assert_eq!(checkpoints[0]["number"], 1);
    assert_eq!(checkpoints[0]["cause"], "removal");
    let checkpoint = fixture.ok(&["work", "checkpoint", "scratch", "--name", "draft"]);
    assert_eq!(checkpoint["number"], 2);
    fs::write(path.join("notes"), "rewritten\n").unwrap();
    let restored = fixture.ok(&["work", "restore", "scratch", "2"]);
    assert_eq!(restored["replaced"]["number"], 3);
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
}

#[test]
fn restore_lists_removed_workspaces_beyond_one_protocol_frame() {
    let fixture = Fixture::new();
    fixture.add("scratch");
    fixture.ok(&["rm", "scratch", "--yes"]);
    // Copies of the record with long checkpoint names add up to more than
    // the 64 KiB frame.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "WITH RECURSIVE copies(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM copies WHERE n < 40)
         INSERT INTO removed_workspaces(workspace_id,repository_id,name,expires_at,record)
         SELECT 'copy-' || n, repository_id, 'copy-' || n, expires_at,
             json_set(record, '$.workspace_id', 'copy-' || n, '$.name', 'copy-' || n,
                 '$.checkpoint.name', replace(hex(zeroblob(1000)), '0', 'x'))
         FROM removed_workspaces, copies",
        [],
    )
    .unwrap();
    let removed = fixture.ok(&["work", "restore"]);
    let names: Vec<_> = removed
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["name"].as_str().unwrap().to_owned())
        .collect();
    let expected: Vec<_> = (1..=40)
        .rev()
        .map(|n| format!("copy-{n}"))
        .chain(["scratch".to_owned()])
        .collect();
    assert_eq!(names, expected);
}

#[test]
fn restore_pickers_reopen_the_picked_workspace_and_return_to_the_picked_checkpoint() {
    let fixture = Fixture::new();
    let picked = fixture.add("picked");
    fixture.add("kept");
    let path = Path::new(picked["path"].as_str().unwrap()).to_owned();
    fs::write(path.join("notes"), "draft\n").unwrap();
    fixture.ok(&["rm", "picked", "--yes"]);
    fixture.ok(&["rm", "kept", "--yes"]);

    let (output, rows) = fixture.pick(&["work", "restore"], picked["id"].as_str().unwrap(), "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[1].starts_with(&format!("{}\t", picked["id"].as_str().unwrap())),
        "{rows:?}"
    );
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
    let removed = fixture.ok(&["work", "restore"]);
    let names: Vec<_> = removed
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["name"])
        .collect();
    assert_eq!(names, ["kept"]);

    fixture.ok(&["work", "checkpoint", "picked", "--name", "draft"]);
    fs::write(path.join("notes"), "rewritten\n").unwrap();
    let (output, rows) = fixture.pick(&["work", "restore", "picked"], "2", "");
    assert!(output.status.success(), "{output:?}");
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[1].starts_with("2\t"), "{rows:?}");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
}

#[test]
fn resume_continues_the_agent_a_removal_stopped_once_restored() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './agent.sh']\nresume_command = ['sh', './resume.sh']\n",
    ));
    let workspace = fixture.add("removed");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(path.join("resume.sh"), "touch resumed\n").unwrap();
    let spawn = |args: &[&str]| {
        fixture
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let agent = spawn(&["claude", "removed"]);
    let command = spawn(&[
        "run",
        "--workspace",
        "removed",
        "--",
        "sh",
        "-c",
        "touch command-started; while :; do sleep 1; done",
    ]);
    assert!(
        try_wait_for(|| (path.join("agent-started").exists()
            && path.join("command-started").exists())
        .then_some(()))
        .is_some(),
        "executions did not start"
    );

    fixture.ok(&["rm", "removed", "--yes"]);
    agent.wait_with_output().unwrap();
    command.wait_with_output().unwrap();
    assert!(!path.exists());
    let removed = fixture.ok(&["work", "restore"]);
    let agents = removed[0]["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{removed}");
    assert_eq!(agents[0]["record"]["agent"], "claude");

    let restored = fixture.ok(&["work", "restore", "removed"]);
    assert_eq!(restored["warnings"], serde_json::json!([]));
    assert_eq!(restored["workspace"]["path"], workspace["path"]);
    let execution = agents[0]["execution"].as_str().unwrap();
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(restored["workspace"]["id"].as_str().unwrap());
    // The interrupted command is not saved, so only the agent resumes.
    let saved = fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".recovery.json") || name.ends_with(".command.json"))
        .collect::<Vec<_>>();
    assert_eq!(saved, [format!("{execution}.recovery.json")]);
    let resumed = fixture.run(&["resume", "removed", "--focus"]);
    assert!(resumed.status.success(), "{resumed:?}");
    assert!(path.join("resumed").exists());
    assert!(!state.join(format!("{execution}.recovery.json")).exists());
}

#[test]
fn removal_accepts_switched_branch_but_refuses_replaced_repository() {
    let fixture = Fixture::new();
    let workspace = fixture.add("switched");
    let path = Path::new(workspace["path"].as_str().unwrap());
    git(path, &["switch", "-c", "my-branch"]);
    fixture.ok(&["rm", "switched"]);
    assert_eq!(
        git(
            &fixture.repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/my-branch"
            ]
        ),
        ""
    );

    let workspace = fixture.add("replaced");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let moved = fixture.root.path().join("original-worktree");
    fs::rename(path, moved).unwrap();
    fs::create_dir(path).unwrap();
    git(path, &["init", "-b", "main"]);
    fs::write(path.join("precious"), "keep me").unwrap();
    let output = fixture.run(&["rm", "replaced"]);
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(path.join("precious")).unwrap(),
        "keep me"
    );
}

#[test]
fn dirty_workspace_is_retained_and_failed_creation_can_be_removed() {
    let fixture = Fixture::new();
    let workspace = fixture.add("dirty");
    let file = Path::new(workspace["path"].as_str().unwrap()).join("untracked");
    fs::write(&file, "keep me").unwrap();
    assert!(!fixture.run(&["rm", "dirty"]).status.success());
    assert_eq!(fs::read_to_string(file).unwrap(), "keep me");
    assert_eq!(
        fixture.ok(&["work", "status", "dirty"])["workspace"]["state"],
        "ready"
    );
    fixture.ok(&["rm", "dirty", "--yes"]);
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert!(
        !fixture
            .run(&[
                "start",
                fixture.repo.to_str().unwrap(),
                "broken",
                "--base",
                "does-not-exist"
            ])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["work", "status", "broken"])["workspace"]["state"],
        "failed"
    );
    fixture.ok(&["rm", "broken"]);
}

#[test]
fn removal_keeps_an_unpushed_branch_without_asking_and_ignores_external_processes() {
    let fixture = Fixture::new();
    let workspace = fixture.add("unpushed");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "different contents").unwrap();
    git(path, &["add", "tracked"]);
    git(
        path,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "unpushed",
        ],
    );
    assert_eq!(fixture.ok(&["rm", "unpushed"])["branch_deleted"], false);
    git(
        &fixture.repo,
        &[
            "rev-parse",
            "--verify",
            workspace["branch"].as_str().unwrap(),
        ],
    );

    let workspace = fixture.add("external");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let mut process = Command::new("sleep")
        .arg("10")
        .current_dir(path)
        .spawn()
        .unwrap();
    let output = fixture.run(&["rm", "external"]);
    let _ = process.kill();
    let _ = process.wait();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!path.exists());
}

#[test]
fn kept_branches_are_listed_and_deleted_on_request() {
    let fixture = Fixture::new();
    for name in ["first", "second", "third"] {
        let workspace = fixture.add(name);
        commit_file(Path::new(workspace["path"].as_str().unwrap()), name, name);
        assert_eq!(fixture.ok(&["rm", name])["branch_deleted"], false);
    }
    let idle = fixture.add("idle");
    let names = |report: &Value| -> Vec<String> {
        report["branches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|branch| branch["branch"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        names(&fixture.ok(&["cleanup", "branches", "--dry-run"])),
        ["first", "second", "third"]
    );
    let branch_exists = |name: &str| {
        !git(&fixture.repo, &["branch", "--list", name])
            .trim()
            .is_empty()
    };

    // --repo resolves an inferred repository name as other commands do.
    let inferred = fixture.repo.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        names(&fixture.ok(&["cleanup", "branches", "--dry-run", "--repo", inferred])),
        ["first", "second", "third"]
    );
    // A dry run lists only the named branches, and refuses unknown ones.
    assert_eq!(
        names(&fixture.ok(&["cleanup", "branches", "second", "--dry-run"])),
        ["second"]
    );
    assert!(
        !fixture
            .run(&["cleanup", "branches", "missing", "--dry-run"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["cleanup", "branches", "first"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["cleanup", "branches", "missing", "--yes"])
            .status
            .success()
    );
    assert!(branch_exists("first"));

    // One picker offers every kind; Tab selects several, and deleting
    // branches asks first.
    let picker = fixture.root.path().join("bin/fzf");
    install_test_script(
        &picker,
        "#!/bin/sh\nawk -F '\\t' '$2 ~ /(first|second) |space      idle/'\n",
    );
    let (output, transcript) = fixture.interactive(&["cleanup"], "y\n");
    assert!(output.status.success(), "{transcript}");
    assert!(transcript.contains("1 unpushed commit"), "{transcript}");
    assert!(!branch_exists("first") && !branch_exists("second"));
    assert!(branch_exists("third"));
    assert!(!Path::new(idle["path"].as_str().unwrap()).exists());

    let deleted = fixture.ok(&["cleanup", "branches", "--all", "--yes"]);
    assert_eq!(names(&deleted), ["third"]);
    assert!(!branch_exists("third"));
    assert!(names(&fixture.ok(&["cleanup", "branches", "--dry-run"])).is_empty());
}

#[test]
fn branch_removal_deletes_branches_main_or_a_remote_has() {
    let fixture = Fixture::new();
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
                "--allow-empty",
                "-m",
                "work",
            ],
        );
    };
    let same = fixture.add("same-tree");
    commit(Path::new(same["path"].as_str().unwrap()));
    assert_eq!(fixture.ok(&["rm", "same-tree"])["branch_deleted"], true);

    let pushed = fixture.add("pushed");
    let path = Path::new(pushed["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "different from main").unwrap();
    commit(path);
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/project.git",
        ],
    );
    git(path, &["update-ref", "refs/remotes/origin/pushed", "HEAD"]);
    git(path, &["branch", "--set-upstream-to=origin/pushed"]);
    // Automatic removal keeps branches while the default branch is unknown.
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    assert_eq!(fixture.ok(&["rm", "pushed"])["branch_deleted"], true);

    // The fake remote above models pushed history, not a fetchable upstream.
    let divergent = fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "divergent",
        "--base",
        "HEAD",
    ]);
    let path = Path::new(divergent["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "unmerged work").unwrap();
    commit(path);
    assert_eq!(fixture.ok(&["rm", "divergent"])["branch_deleted"], false);
    assert!(!path.exists());
}

#[test]
fn a_parent_removes_a_child_an_interrupted_split_left_preparing() {
    let fixture = Fixture::new();
    fixture.add("parent");
    fixture.add("other");
    let split = scoped_command(&fixture, "parent", &["split", "child"]);
    assert!(split.status.success(), "{split:?}");
    // The split was interrupted before the child's setup started.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute(
        "UPDATE workspaces SET state='preparing' WHERE name='child'",
        [],
    )
    .unwrap();
    let again = scoped_command(&fixture, "parent", &["split", "child"]);
    assert!(!again.status.success(), "{again:?}");
    for name in ["parent", "other"] {
        let output = scoped_command(&fixture, "parent", &["rm", name, "--yes"]);
        assert!(!output.status.success(), "{name}: {output:?}");
    }
    // A held child is the user's: a batch naming it removes none.
    let held = scoped_command(&fixture, "parent", &["split", "held"]);
    assert!(held.status.success(), "{held:?}");
    fixture.ok(&["acquire", "hold", "--workspace", "held", "--name", "review"]);
    let refused = scoped_command(&fixture, "parent", &["rm", "child", "held", "--yes"]);
    assert!(!refused.status.success(), "{refused:?}");
    assert_eq!(
        fixture.ok(&["work", "status", "child"])["workspace"]["state"],
        "preparing"
    );
    let removed = scoped_command(&fixture, "parent", &["rm", "child", "--yes"]);
    assert!(removed.status.success(), "{removed:?}");
    let again = scoped_command(&fixture, "parent", &["split", "child"]);
    assert!(again.status.success(), "{again:?}");
    assert_eq!(
        fixture.ok(&["work", "status", "child"])["workspace"]["parent"]["name"],
        "parent"
    );
    assert_eq!(
        fixture.ok(&["work", "status", "other"])["workspace"]["name"],
        "other"
    );
}

#[test]
fn removal_confirmation_says_when_the_checkpoint_cannot_keep_the_files() {
    let fixture = Fixture::with_config(Some("[cleanup.restore]\narchive_max_mb = 0\n"));
    let workspace = fixture.add("large");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("uncommitted"), "too large to archive").unwrap();
    let (no, prompt) = fixture.interactive(&["rm", "large"], "n\n");
    assert!(!no.status.success());
    assert!(prompt.contains("cannot be restored"), "{prompt}");
    assert!(path.join("uncommitted").exists());
}

#[test]
fn interactive_removal_confirms_and_defaults_to_no_without_affecting_scripts() {
    let fixture = Fixture::new();
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    let workspace = fixture.add("worker");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("uncommitted"), "keep until approved").unwrap();
    let piped = fixture.run(&["rm", "worker"]);
    assert!(!piped.status.success());
    let (no, prompt) = fixture.interactive(&["rm", "worker"], "n\n");
    assert!(!no.status.success());
    // Changes small enough for the checkpoint can be restored.
    assert!(!prompt.contains("cannot be restored"), "{prompt}");
    assert!(path.join("uncommitted").exists());
    // An answer that is neither is asked again.
    let (yes, prompt) = fixture.interactive(&["rm", "worker"], "maybe\ny\n");
    assert!(yes.status.success(), "{prompt}");
    assert!(!path.exists());
    // The branch had no commits of its own.
    let branch_exists = git(
        &fixture.repo,
        &["branch", "--list", workspace["branch"].as_str().unwrap()],
    );
    assert!(branch_exists.trim().is_empty());
    for answer in ["n\n", "\n", "\x04"] {
        let (no, prompt) = fixture.interactive(&["repo", "rm", "project"], answer);
        assert!(!no.status.success(), "{prompt}");
        assert!(fixture.repo.exists());
    }
    let (json, _) = fixture.interactive(&["--json", "repo", "rm", "project"], "y\n");
    assert!(!json.status.success());
    assert!(fixture.repo.exists());
    let (yes, prompt) = fixture.interactive(&["repo", "rm", "project"], "Y\n");
    assert!(yes.status.success(), "{prompt}");
    assert!(!fixture.repo.exists());
}

#[test]
fn removal_of_several_workspaces_confirms_once_and_checks_all_first() {
    let fixture = Fixture::new();
    let names = ["first", "second", "third"];
    let workspaces: Vec<Value> = names.iter().map(|name| fixture.add(name)).collect();
    let path = |index: usize| PathBuf::from(workspaces[index]["path"].as_str().unwrap());
    for index in [0, 2] {
        fs::write(path(index).join("uncommitted"), "unsaved").unwrap();
    }
    let refused = fixture.run(&["rm", "first", "missing", "--yes"]);
    assert!(!refused.status.success());
    assert!(path(0).exists());
    // So does one in another lifecycle operation, which the check accepts.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let set_state = |state: &str| {
        db.execute(
            "UPDATE workspaces SET state=?1 WHERE name='second'",
            [state],
        )
        .unwrap();
    };
    set_state("updating");
    let refused = fixture.run(&["rm", "first", "second", "--yes"]);
    assert!(!refused.status.success());
    assert!(path(0).exists());
    set_state("ready");
    // And one whose removal closes this tab, ending the removal of the others.
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspaces[1]["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("closing-herdr-w1:p7"), "").unwrap();
    let refused = fixture
        .command()
        .args(["rm", "first", "second", "--yes"])
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p7")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(path(0).exists());
    fs::remove_file(state.join("closing-herdr-w1:p7")).unwrap();

    let (no, prompt) = fixture.interactive(&["rm", "first", "second", "third"], "n\n");
    let prompt = prompt.replace("\r\n", "\n");
    assert!(!no.status.success(), "{prompt}");
    assert_eq!(prompt.matches("Are you sure? [y/N]").count(), 1, "{prompt}");
    assert!(prompt.contains("first:\n  Deletes: "), "{prompt}");
    assert!(prompt.contains("third:\n  Deletes: "), "{prompt}");
    assert!(!prompt.contains("second:"), "{prompt}");
    assert!((0..3).all(|index| path(index).exists()));

    let (picked, prompt) = fixture.pick(
        &["rm"],
        &format!(
            "{} {}",
            workspaces[0]["id"].as_str().unwrap(),
            workspaces[1]["id"].as_str().unwrap()
        ),
        "y\n",
    );
    assert!(picked.status.success(), "{prompt}");
    assert!(!path(0).exists() && !path(1).exists() && path(2).exists());

    let second = fixture.add("fourth");
    // One still preparing, whose setup never started, goes with the others.
    db.execute(
        "UPDATE workspaces SET state='preparing' WHERE name='fourth'",
        [],
    )
    .unwrap();
    // A workspace named by its name and its ID is removed once.
    let third = workspaces[2]["id"].as_str().unwrap();
    let removed = fixture.ok(&["rm", "third", third, "fourth", "--yes"]);
    let removed_names: Vec<&str> = removed["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|removal| removal["workspace"].as_str().unwrap())
        .collect();
    assert_eq!(removed_names, ["third", "fourth"]);
    assert_eq!(removed["failed"], serde_json::json!([]));
    assert!(!path(2).exists());
    assert!(!Path::new(second["path"].as_str().unwrap()).exists());
}

#[test]
fn removal_confirmation_lists_git_changes_before_asking() {
    let fixture = Fixture::new();
    let workspace = fixture.add("preview");
    let path = Path::new(workspace["path"].as_str().unwrap());
    for name in ["modified", "deleted"] {
        fs::write(path.join(name), "original\n").unwrap();
    }
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
            "Files for removal preview",
        ],
    );
    git(path, &["mv", "tracked", "renamed file"]);
    fs::write(path.join("modified"), "changed\n").unwrap();
    fs::remove_file(path.join("deleted")).unwrap();
    fs::write(path.join("added"), "staged\n").unwrap();
    git(path, &["add", "added"]);
    fs::create_dir(path.join("new directory")).unwrap();
    fs::write(path.join("new directory/child"), "untracked\n").unwrap();
    fs::write(path.join("line\nbreak"), "untracked\n").unwrap();
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "ignored\n").unwrap();
    git(path, &["config", "status.relativePaths", "true"]);
    git(path, &["config", "status.showUntrackedFiles", "no"]);

    let (no, prompt) = fixture.interactive(&["rm", "preview"], "n\n");
    assert!(!no.status.success(), "{prompt}");
    let confirmation = prompt.find("Are you sure? [y/N]").unwrap();
    for entry in [
        "A  added",
        " D deleted",
        " M modified",
        "R  tracked -> \"renamed file\"",
        "?? \"new directory/child\"",
        "?? \"line\\nbreak\"",
    ] {
        let position = prompt.find(entry).unwrap_or_else(|| panic!("{prompt}"));
        assert!(position < confirmation, "{prompt}");
    }
    assert!(!prompt.contains("ignored/cache"), "{prompt}");
    assert_eq!(
        fs::read_to_string(path.join("modified")).unwrap(),
        "changed\n"
    );
    assert!(path.join("new directory/child").exists());
    let removed = fixture.run(&["rm", "preview", "--yes"]);
    assert!(removed.status.success());
    assert!(!path.exists());
}

#[test]
fn removal_preview_limits_large_untracked_directories() {
    let fixture = Fixture::new();
    let workspace = fixture.add("large-preview");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::create_dir(path.join("untracked")).unwrap();
    for index in 0..500 {
        fs::write(
            path.join(format!("untracked/{index:03}-{}", "x".repeat(140))),
            "keep until approved",
        )
        .unwrap();
    }
    let (no, prompt) = fixture.interactive(&["rm", "large-preview"], "n\n");
    assert!(!no.status.success(), "{prompt}");
    assert_eq!(prompt.matches("?? untracked/").count(), 50, "{prompt}");
    assert!(prompt.contains("450 more"), "{prompt}");
    assert_eq!(fs::read_dir(path.join("untracked")).unwrap().count(), 500);
    let piped = fixture.run(&["rm", "large-preview"]);
    assert!(!piped.status.success());
    let error = String::from_utf8_lossy(&piped.stderr);
    assert!(!error.contains("?? untracked/"), "{error}");
    fixture.ok(&["rm", "large-preview", "--yes"]);
    assert!(!path.exists());
}

// Fail one Git operation while leaving fixture setup and verification on real Git.
fn install_failing_git(fixture: &Fixture) {
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file() && path.metadata().unwrap().permissions().mode() & 0o111 != 0)
        .unwrap()
        .canonicalize()
        .unwrap();
    // The macOS developer-tool shim resolves the tool by its invoked basename.
    let real_bin = bin.join("real");
    fs::create_dir(&real_bin).unwrap();
    std::os::unix::fs::symlink(real_git, real_bin.join("git")).unwrap();
    install_test_script(
        bin.join("git"),
        r#"#!/bin/sh
if [ -f "$HOME/git-failure" ]; then
    failure=$(cat "$HOME/git-failure")
    for arg; do
        if [ "$arg" = "$failure" ]; then
            echo "injected Git predicate failure" >&2
            exit 128
        fi
    done
fi
exec "$HOME/bin/real/git" "$@"
"#,
    );
}

#[test]
fn daemon_git_predicate_failures_preserve_branches_and_workspaces() {
    let fixture = Fixture::new();
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    upstream_remote(&fixture);
    // Synced first, so the daemon's own syncs leave main where it is.
    fixture.ok(&["repo", "sync", fixture.repo.to_str().unwrap()]);
    // Make the default's tree differ so removal must check ancestry.
    fs::write(path.join("tracked"), "preserved work\n").unwrap();
    git(path, &["add", "tracked"]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "work",
        ],
    );
    let before = git(&fixture.repo, &["rev-parse", "main"]);
    install_failing_git(&fixture);
    for (failure, args) in [
        ("--is-ancestor", vec!["merge", "worker"]),
        (
            "show-ref",
            vec![
                "start",
                fixture.repo.to_str().unwrap(),
                "--existing",
                "main",
            ],
        ),
        ("--is-ancestor", vec!["rm", "worker", "--yes"]),
        ("for-each-ref", vec!["rm", "worker", "--yes"]),
    ] {
        fs::write(fixture.root.path().join("git-failure"), failure).unwrap();
        let output = fixture.run(&args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("injected Git predicate failure"),
            "{args:?}: {error}"
        );
        assert!(!error.contains("diverged"), "{error}");
        assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
        assert!(path.is_dir());
        assert_eq!(
            fs::read_to_string(path.join("tracked")).unwrap(),
            "preserved work\n"
        );
        assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
    }
}

#[test]
fn holds_are_visible_and_manual_removal_confirms_holders() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("held");
    let hold = fixture.ok(&[
        "acquire",
        "hold",
        "--workspace",
        "held",
        "--name",
        "app-thread",
        "--reason",
        "Open thread",
    ]);
    assert_eq!(hold["workspace_id"], workspace["id"]);
    let expected = serde_json::json!([hold]);
    assert_eq!(
        fixture.ok(&["work", "status", "held"])["workspace"]["holds"],
        expected
    );
    assert_eq!(fixture.ok(&["work"])[0]["holds"], expected);
    assert_eq!(
        fixture.ok(&["leases", "hold", "--workspace", "held"]),
        expected
    );
    let empty = fixture.run(&["release", "--workspace", "held"]);
    assert!(!empty.status.success(), "{empty:?}");
    assert_eq!(
        fixture.ok(&["leases", "hold", "--workspace", "held"]),
        expected
    );
    assert_eq!(
        fixture.ok(&["leases", "hold", "--all"])[0]["holds"],
        expected
    );
    // Workspace processes list their holds, ask to hold their own workspace
    // and release only the holds agents placed.
    let scoped = |args: &[&str]| {
        let mut command = vec![
            "run",
            "--workspace",
            "held",
            "--",
            support::shoal_binary(),
            "--json",
        ];
        command.extend_from_slice(args);
        fixture.run(&command)
    };
    let listed = scoped(&["leases", "hold"]);
    assert!(listed.status.success(), "{listed:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.stdout).unwrap(),
        expected
    );
    let unexplained = scoped(&["acquire", "hold", "--name", "scoped"]);
    assert!(!unexplained.status.success());
    let requested = scoped(&[
        "acquire",
        "hold",
        "--name",
        "scoped",
        "--reason",
        "Keep my notes",
    ]);
    assert_eq!(requested.status.code(), Some(2), "{requested:?}");
    assert!(
        format!("{requested:?}").contains("approval_pending"),
        "{requested:?}"
    );
    let release = scoped(&["release", "hold", "--name", "app-thread"]);
    assert!(!release.status.success());
    fixture.restart();
    assert_eq!(
        fixture.ok(&["leases", "hold", "--workspace", "held"]),
        expected
    );
    let (no, prompt) = fixture.interactive(&["rm", "held"], "n\n");
    assert!(!no.status.success());
    assert!(prompt.contains("app-thread"), "{prompt}");
    let removed = fixture.ok(&["rm", "held", "--yes"]);
    assert_eq!(removed["holds"], expected);
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM workspace_holds", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let missing = fixture.add("missing-held");
    // A commit of its own keeps the branch from the kept-branch sweep.
    commit_file(
        Path::new(missing["path"].as_str().unwrap()),
        "missing-held",
        "work",
    );
    fixture.ok(&[
        "acquire",
        "hold",
        "--workspace",
        "missing-held",
        "--name",
        "app",
    ]);
    fs::remove_dir_all(missing["path"].as_str().unwrap()).unwrap();
    fixture.restart();
    wait_removed(&fixture, "missing-held");
    assert!(!git(&fixture.repo, &["branch", "--list", "missing-held"]).is_empty());
    assert_eq!(
        db.query_row("SELECT count(*) FROM workspace_holds", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn removing_a_parent_lists_and_removes_its_children() {
    let fixture = Fixture::new();
    fixture.add("parent");
    let child = fixture.ok(&["split", "child", "--workspace", "parent"]);
    let child_path = Path::new(child["path"].as_str().unwrap()).to_owned();
    // A clean parent still asks, naming the children it takes.
    let piped = fixture.run(&["rm", "parent"]);
    assert!(!piped.status.success());
    let (no, prompt) = fixture.interactive(&["rm", "parent"], "n\n");
    assert!(!no.status.success());
    assert!(prompt.contains("child"), "{prompt}");
    assert!(child_path.exists());
    let (yes, prompt) = fixture.interactive(&["rm", "parent"], "y\n");
    assert!(yes.status.success(), "{prompt}");
    assert!(!child_path.exists());
    assert!(!fixture.run(&["work", "status", "child"]).status.success());

    fixture.add("parent");
    fixture.ok(&["split", "child", "--workspace", "parent"]);
    let removed = fixture.ok(&["rm", "parent", "--yes"]);
    assert_eq!(
        removed["children"],
        serde_json::json!(["child"]),
        "{removed}"
    );
}

#[test]
fn a_disposable_workspace_goes_whatever_it_holds_without_a_checkpoint() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let workspace = fixture.ok(&["start", repo, "scratch", "--disposable"]);
    assert_eq!(workspace["disposable"], true);
    let path = PathBuf::from(workspace["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "changed\n").unwrap();
    git(
        &path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qam",
            "unpushed",
        ],
    );
    fs::write(path.join("untracked"), "draft\n").unwrap();
    let child = fixture.ok(&[
        "split",
        "--no-branch",
        "--workspace",
        "scratch",
        "--disposable",
    ]);
    let child_path = PathBuf::from(child["path"].as_str().unwrap());
    fs::write(child_path.join("untracked"), "draft\n").unwrap();

    // No confirmation: nothing in it is worth keeping.
    let removed = fixture.ok(&["rm", "scratch"]);
    assert_eq!(removed["branch_deleted"], true, "{removed}");
    assert!(!path.exists() && !child_path.exists());
    assert!(git(&fixture.repo, &["branch", "--list", "scratch"]).is_empty());
    assert_eq!(fixture.ok(&["work", "restore"]), serde_json::json!([]));

    // Its removal keeps nothing to export, which refuses a batch before
    // removing any of it.
    fixture.add("kept");
    fixture.ok(&["start", repo, "scratch-2", "--disposable"]);
    let directory = fixture.root.path().join("exports");
    let refused = fixture.run(&[
        "move",
        "export",
        "--repo",
        repo,
        "--all",
        "-o",
        directory.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!refused.status.success());
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 2);
}
