use crate::support::{Fixture, git, upstream_remote};
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Stdio};

#[test]
fn named_workspace_uses_committed_history_and_deletes_redundant_branch() {
    let fixture = Fixture::new();
    let first = fixture.ok(&["repo", "list"]);
    fixture.ok(&["repo", "add", fixture.repo.to_str().unwrap()]);
    assert_eq!(fixture.ok(&["repo", "list"]), first);
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
fn concurrent_adds_cannot_claim_the_same_name() {
    let fixture = Fixture::new();
    for (first_name, second_name, workspace_name) in [
        ("shared", "shared", "shared"),
        ("shared/topic", "shared-topic", "shared-topic"),
    ] {
        let first = fixture
            .command()
            .args(["add", fixture.repo.to_str().unwrap(), first_name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let second = fixture.run(&["add", fixture.repo.to_str().unwrap(), second_name]);
        let first = first.wait_with_output().unwrap();
        assert_ne!(first.status.success(), second.status.success());
        assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
        fixture.ok(&["rm", workspace_name]);
    }
}

#[test]
fn branch_names_are_preserved_with_portable_workspace_names() {
    let fixture = Fixture::new();
    let long = format!("long/{}", "x".repeat(100));
    let sha1 = "a".repeat(40);
    let sha256 = "b".repeat(64);
    for (branch, name) in [
        (
            "henrik/8374-set-league-season-player-profile",
            "henrik-8374-set-league-season-player-profile",
        ),
        ("release/v1.2", "release-v1-2"),
        ("feature/æøå-日本語", "feature--------"),
        ("_private", "private"),
        ("+special", "special"),
        ("@", "workspace"),
        (sha1.as_str(), sha1.as_str()),
        (sha256.as_str(), sha256.as_str()),
        ("refs/heads/topic", "refs-heads-topic"),
        ("topic/-leaf", "topic--leaf"),
        ("\u{2003}topic\u{2003}", "topic-"),
        (
            "topic/quote'\";$`(literal)&{ok}",
            "topic-quote------literal---ok-",
        ),
        (long.as_str(), &format!("long-{}", "x".repeat(59))),
    ] {
        git(&fixture.repo, &["check-ref-format", "--branch", branch]);
        let workspace = fixture.add(branch);
        let actual_branch = if branch == "@" || branch == sha1 || branch == sha256 {
            format!("{branch}-2")
        } else {
            branch.to_owned()
        };
        assert_eq!(workspace["branch"], actual_branch);
        assert_eq!(workspace["name"], name, "{branch}");
        let path = Path::new(workspace["path"].as_str().unwrap());
        assert_eq!(path.file_name().unwrap(), name);
        assert_eq!(
            git(path, &["symbolic-ref", "HEAD"]),
            format!("refs/heads/{actual_branch}\n")
        );
        assert_eq!(
            fixture.ok(&["inspect", name])["workspace"]["id"],
            workspace["id"]
        );
        let output = fixture.run(&["exec", name, "--", "git", "symbolic-ref", "HEAD"]);
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            format!("refs/heads/{actual_branch}\n").as_bytes()
        );
        assert_eq!(fixture.ok(&["merge", "main", name])["success"], true);
        assert_eq!(fixture.ok(&["rm", name])["branch_deleted"], true);
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
                .run(&["add", fixture.repo.to_str().unwrap(), branch])
                .status
                .success(),
            "{branch}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
    git(&fixture.repo, &["branch", "-d", "previous"]);
    let existing = fixture.add("feature/topic");
    for branch in ["feature-topic", "feature.topic"] {
        let output = fixture.run(&["add", fixture.repo.to_str().unwrap(), branch]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("feature-topic"));
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

#[test]
fn interactive_add_preserves_literal_branch_spelling() {
    let fixture = Fixture::new();
    let (output, transcript) = fixture.interactive(
        &["add", fixture.repo.to_str().unwrap(), "--base", "main"],
        "\u{2003}henrik/topic\u{2003}\n",
    );
    assert!(output.status.success(), "{transcript}");
    assert!(transcript.contains("Branch name:"));
    let workspace = &fixture.ok(&["inspect", "henrik-topic-"])["workspace"];
    assert_eq!(workspace["branch"], "\u{2003}henrik/topic\u{2003}");
    fixture.ok(&["rm", "henrik-topic-"]);
}

#[test]
fn add_uses_develop_as_the_repository_default() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["branch", "-m", "develop"]);
    let author = upstream_remote(&fixture);
    let expected = git(&author, &["rev-parse", "HEAD"]);
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
        (
            "origin/topic",
            source.trim(),
            Some("refs/remotes/origin/topic"),
        ),
        ("v1", source.trim(), Some("refs/tags/v1")),
        (source.trim(), source.trim(), None),
        ("feature/source~1", initial.trim(), None),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("from-base-{index}");
        let workspace = fixture.ok(&["add", fixture.repo.to_str().unwrap(), &name, "--base", base]);
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
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), initial);
    assert_eq!(git(&fixture.repo, &["rev-parse", "feature/source"]), source);
}

#[test]
fn add_refreshes_main_before_creating_the_worktree() {
    let fixture = Fixture::new();
    let author = upstream_remote(&fixture);
    let expected = git(&author, &["rev-parse", "HEAD"]);
    let workspace = fixture.add("fresh");
    let path = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), expected);
    assert_eq!(git(path, &["rev-parse", "HEAD"]), expected);
    assert_eq!(workspace["base_ref"], "refs/heads/main");
    assert_eq!(workspace["base_commit"], expected.trim());
    assert_eq!(
        fs::read_to_string(path.join("upstream")).unwrap(),
        "from remote\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("upstream")).unwrap(),
        "from remote\n"
    );
}

#[test]
fn add_existing_branch_runs_setup_once_and_denies_scoped_creation() {
    let fixture = Fixture::new();
    for hook in ["setup", "post"] {
        let path = fixture.repo.join(hook);
        fs::write(&path, format!("#!/bin/sh\necho {hook} >> runs\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "setup_cmd = './setup'\npost_setup_cmd = './post'\n",
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
        "add",
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
    let output = fixture.run(&[
        "exec",
        "coworker",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "add",
        fixture.repo.to_str().unwrap(),
        "--existing",
        "main",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("workspace processes"),
        "{output:?}"
    );
    assert!(
        !fixture
            .run(&[
                "add",
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
                    "add",
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
    use std::os::fd::FromRawFd;
    let fixture = Fixture::new();
    git(&fixture.repo, &["branch", "coworker/topic"]);
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let picker = bin.join("fzf");
    fs::write(&picker, "#!/bin/sh\nawk -F '\\t' '$2 == \"Use an existing branch\" || $1 == \"refs/heads/coworker/topic\" {print}'\n").unwrap();
    fs::set_permissions(&picker, fs::Permissions::from_mode(0o755)).unwrap();
    let directive = fixture.root.path().join("destination");
    for _ in 0..2 {
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let _master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let output = fixture
            .command()
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .args(["add", fixture.repo.to_str().unwrap()])
            .stdin(slave.try_clone().unwrap())
            .stderr(slave)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let workspace = fixture.ok(&["inspect", "coworker-topic"]);
        assert_eq!(
            fs::read_to_string(&directive).unwrap().trim(),
            workspace["workspace"]["path"].as_str().unwrap()
        );
    }
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

#[test]
fn one_off_workspace_paths_preserve_repository_defaults_and_ownership() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let destination = fixture
        .root
        .path()
        .join("elsewhere/one off ' {{ branch }} path");
    let path = destination.to_str().unwrap();
    let workspace = fixture.ok(&["add", repo, "custom/topic", "--path", path]);
    assert_eq!(
        Path::new(workspace["path"].as_str().unwrap()),
        fs::canonicalize(path).unwrap()
    );
    assert_eq!(workspace["name"], "custom-topic");
    assert_eq!(
        fixture.ok(&["add", repo, "--existing", "custom/topic", "--path", path])["id"],
        workspace["id"]
    );
    assert!(
        !fixture
            .run(&[
                "add",
                repo,
                "--existing",
                "custom/topic",
                "--path",
                fixture.root.path().join("different").to_str().unwrap()
            ])
            .status
            .success()
    );
    let ordinary = fixture.add("ordinary");
    assert!(Path::new(ordinary["path"].as_str().unwrap()).starts_with(fixture.shoal_dir()));
    fixture.ok(&["rm", "custom-topic", "--keep-branch", "--yes"]);
    assert!(!destination.exists());
    assert!(destination.parent().unwrap().exists());
    git(&fixture.repo, &["branch", "existing-path"]);
    fixture.ok(&["add", repo, "--existing", "existing-path", "--path", path]);
}

#[test]
fn one_off_workspace_paths_reject_existing_and_protected_directories() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let owned = fixture.add("owned");
    let owned = Path::new(owned["path"].as_str().unwrap());
    let alias = fixture.root.path().join("alias");
    symlink(&fixture.repo, &alias).unwrap();
    let occupied = fixture.root.path().join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep"), "precious").unwrap();
    for path in [
        occupied.clone(),
        fixture.repo.join("nested"),
        alias.join("nested"),
        fixture.root.path().join("state/nested"),
        owned.join("nested"),
        owned.parent().unwrap().to_path_buf(),
        fixture.root.path().to_path_buf(),
    ] {
        let output = fixture.run(&["add", repo, "rejected", "--path", path.to_str().unwrap()]);
        assert!(!output.status.success(), "accepted {}", path.display());
    }
    assert_eq!(
        fs::read_to_string(occupied.join("keep")).unwrap(),
        "precious"
    );
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
    assert!(!git(&fixture.repo, &["branch", "--list", "rejected"]).contains("rejected"));
}

#[test]
fn adopt_cli_preserves_work_and_uses_normal_lifecycle() {
    let mut fixture = Fixture::new();
    let path = fixture.root.path().join("existing worktree");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "adopt/topic",
            path.to_str().unwrap(),
        ],
    );
    fs::write(path.join("tracked"), "work in progress\n").unwrap();
    fs::write(
        path.join(".shoal.toml"),
        "setup_cmd = 'missing'\npost_setup_cmd = 'missing'\ngit_profile = 'missing'\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args([
            "--json",
            "adopt",
            fixture.repo.to_str().unwrap(),
            "existing worktree",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let w: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(w["name"], "adopt-topic");
    assert_eq!(w["state"], "ready");
    fixture.restart();
    let again = fixture.ok(&[
        "adopt",
        fixture.repo.to_str().unwrap(),
        "~/existing worktree",
    ]);
    assert_eq!(again["id"], w["id"]);
    let output = fixture.run(&["exec", "adopt-topic", "--", "cat", "tracked"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "work in progress\n"
    );
    let diff = fixture.run(&["diff", "adopt-topic"]);
    assert!(diff.status.success());
    assert!(String::from_utf8_lossy(&diff.stdout).contains("+work in progress"));
    // Full ownership includes resource release and repository removal of this external path.
    fixture.ok(&["port", "acquire", "test", "adopt-topic"]);
    fixture.ok(&["repo", "rm", fixture.repo.to_str().unwrap(), "--yes"]);
    assert!(!path.exists());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
}

#[test]
fn explicit_locations_do_not_require_unrelated_checkouts_to_be_readable() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let offline = fixture.root.path().join("offline-repo");
    fs::create_dir(&offline).unwrap();
    git(&offline, &["init", "-b", "main"]);
    let registration = fixture.ok(&["repo", "add", offline.to_str().unwrap()]);
    fs::remove_dir_all(&offline).unwrap();
    let destination = fixture.root.path().join("custom");
    fixture.ok(&[
        "add",
        repo,
        "custom",
        "--path",
        destination.to_str().unwrap(),
    ]);
    let adopted = fixture.root.path().join("adopted");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "adopted",
            adopted.to_str().unwrap(),
        ],
    );
    fixture.ok(&["adopt", repo, adopted.to_str().unwrap()]);
    for protected in [
        offline.join("nested"),
        Path::new(registration["workspaces_dir"].as_str().unwrap()).join("nested"),
    ] {
        assert!(
            !fixture
                .run(&[
                    "add",
                    repo,
                    "rejected",
                    "--path",
                    protected.to_str().unwrap()
                ])
                .status
                .success()
        );
    }
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 2);
}
