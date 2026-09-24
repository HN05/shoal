use crate::support::{Fixture, git};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

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
    assert!(String::from_utf8_lossy(&output.stderr).contains("different repository"));
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
        fixture.ok(&["inspect", "dirty"])["workspace"]["state"],
        "ready"
    );
    fixture.ok(&["rm", "dirty", "--yes", "--keep-branch"]);
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert!(
        !fixture
            .run(&[
                "add",
                fixture.repo.to_str().unwrap(),
                "broken",
                "--base",
                "does-not-exist"
            ])
            .status
            .success()
    );
    assert_eq!(
        fixture.ok(&["inspect", "broken"])["workspace"]["state"],
        "failed"
    );
    fixture.ok(&["rm", "broken"]);
}

#[test]
fn removal_requires_branch_choice_for_differences_but_not_external_processes() {
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
    let output = fixture.run(&["rm", "unpushed"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("branch choice"));
    fixture.ok(&["rm", "unpushed", "--yes", "--keep-branch"]);
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
fn branch_removal_compares_contents_to_main_or_upstream_and_honors_explicit_choice() {
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
    assert_eq!(fixture.ok(&["rm", "pushed"])["branch_deleted"], true);

    // The fake remote above models pushed history, not a fetchable upstream.
    let divergent = fixture.ok(&[
        "add",
        fixture.repo.to_str().unwrap(),
        "divergent",
        "--base",
        "HEAD",
    ]);
    let path = Path::new(divergent["path"].as_str().unwrap());
    fs::write(path.join("tracked"), "unmerged work").unwrap();
    commit(path);
    assert!(!fixture.run(&["rm", "divergent"]).status.success());
    assert!(path.exists());
    assert_eq!(
        fixture.ok(&["rm", "divergent", "--yes", "--delete-branch"])["branch_deleted"],
        true
    );
}

#[test]
fn removal_picker_keeps_or_deletes_the_selected_branch_and_can_cancel() {
    let fixture = Fixture::new();
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let picker = bin.join("fzf");
    for (name, label) in [
        ("cancel-choice", "Cancel"),
        ("keep-choice", "Keep branch"),
        ("delete-choice", "Delete branch"),
    ] {
        let workspace = fixture.add(name);
        let path = Path::new(workspace["path"].as_str().unwrap());
        fs::write(path.join("uncommitted"), "requires a branch choice").unwrap();
        fs::write(
            &picker,
            format!("#!/bin/sh\nawk -F '\\t' '$2 ~ /^{label} / {{ print }}'\n"),
        )
        .unwrap();
        fs::set_permissions(&picker, fs::Permissions::from_mode(0o755)).unwrap();
        let (output, transcript) = fixture.interactive(&["rm", name], "y\n");
        assert_eq!(output.status.success(), label != "Cancel", "{transcript}");
        assert_eq!(path.exists(), label == "Cancel");
        let branch = git(
            &fixture.repo,
            &["branch", "--list", workspace["branch"].as_str().unwrap()],
        );
        assert_eq!(!branch.trim().is_empty(), label != "Delete branch");
        if label == "Cancel" {
            assert!(transcript.contains("removal canceled"), "{transcript}");
            assert!(path.join("uncommitted").exists());
        }
    }
}

#[test]
fn interactive_removal_confirms_and_defaults_to_no_without_affecting_scripts() {
    let fixture = Fixture::new();
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    for flag in ["--keep-branch", "--delete-branch"] {
        let workspace = fixture.add("worker");
        let path = Path::new(workspace["path"].as_str().unwrap());
        fs::write(path.join("uncommitted"), "keep until approved").unwrap();
        let piped = fixture.run(&["rm", "worker", flag]);
        assert!(!piped.status.success());
        assert!(String::from_utf8_lossy(&piped.stderr).contains("pass --yes"));
        let (no, prompt) = fixture.interactive(&["rm", "worker", flag], "n\n");
        assert!(!no.status.success());
        assert!(prompt.contains("Are you sure? [y/N]"), "{prompt}");
        assert!(path.join("uncommitted").exists());
        let (yes, prompt) = fixture.interactive(&["rm", "worker", flag], "maybe\ny\n");
        assert!(yes.status.success(), "{prompt}");
        assert!(prompt.contains("Please enter y or n."), "{prompt}");
        assert!(!path.exists());
        let branch_exists = git(
            &fixture.repo,
            &["branch", "--list", workspace["branch"].as_str().unwrap()],
        );
        assert_eq!(!branch_exists.trim().is_empty(), flag == "--keep-branch");
    }
    for answer in ["n\n", "\n", "\x04"] {
        let (no, prompt) = fixture.interactive(&["repo", "rm", "project"], answer);
        assert!(!no.status.success(), "{prompt}");
        assert!(prompt.contains("Are you sure? [y/N]"), "{prompt}");
        assert!(fixture.repo.exists());
    }
    let (json, prompt) = fixture.interactive(&["--json", "repo", "rm", "project"], "y\n");
    assert!(!json.status.success());
    assert!(!prompt.contains("Are you sure?"));
    assert!(fixture.repo.exists());
    let (yes, prompt) = fixture.interactive(&["repo", "rm", "project"], "Y\n");
    assert!(yes.status.success(), "{prompt}");
    assert!(!fixture.repo.exists());
}
