//! Adopting existing checkouts, and workspaces at one-off paths.
use super::*;

#[test]
fn one_off_workspace_paths_preserve_repository_defaults_and_ownership() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let destination = fixture
        .root
        .path()
        .join("elsewhere/one off ' {{ branch }} path");
    let path = destination.to_str().unwrap();
    let workspace = fixture.ok(&["start", repo, "custom/topic", "--path", path]);
    assert_eq!(
        Path::new(workspace["path"].as_str().unwrap()),
        fs::canonicalize(path).unwrap()
    );
    assert_eq!(workspace["name"], "custom-topic");
    assert_eq!(
        fixture.ok(&["start", repo, "--existing", "custom/topic", "--path", path])["id"],
        workspace["id"]
    );
    assert!(
        !fixture
            .run(&[
                "start",
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
    fixture.ok(&["rm", "custom-topic", "--yes"]);
    assert!(!destination.exists());
    assert!(destination.parent().unwrap().exists());
    git(&fixture.repo, &["branch", "existing-path"]);
    fixture.ok(&["start", repo, "--existing", "existing-path", "--path", path]);
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
        let output = fixture.run(&["start", repo, "rejected", "--path", path.to_str().unwrap()]);
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
        "setup.command = 'missing'\nhooks.post_setup = 'missing'\ngit_profile = 'missing'\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["--json", "adopt", "existing worktree"])
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
    assert_eq!(w["base_ref"], "refs/heads/main");
    assert_eq!(
        w["base_commit"],
        git(&fixture.repo, &["rev-parse", "main"]).trim()
    );
    fixture.restart();
    let again = fixture.ok(&[
        "adopt",
        "~/existing worktree",
        "--repo",
        fixture.repo.to_str().unwrap(),
    ]);
    assert_eq!(again["id"], w["id"]);
    let output = fixture.run(&["run", "--workspace", "adopt-topic", "--", "cat", "tracked"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "work in progress\n"
    );
    let diff = fixture.run(&["diff", "adopt-topic"]);
    assert!(diff.status.success());
    assert!(String::from_utf8_lossy(&diff.stdout).contains("+work in progress"));
    // Full ownership includes resource release and repository removal of this external path.
    fixture.ok(&["acquire", "port", "test", "--workspace", "adopt-topic"]);
    fixture.ok(&["repo", "rm", fixture.repo.to_str().unwrap(), "--yes"]);
    assert!(!path.exists());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
}

#[test]
fn adopt_copy_copies_dirty_and_untracked_work_to_the_default_directory() {
    let fixture = Fixture::new();
    let source = fixture.root.path().join("external worktree");
    let repo = fixture.repo.to_str().unwrap();
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "copy/topic",
            source.to_str().unwrap(),
        ],
    );
    // A tracked file deleted from a directory that remains stays deleted.
    fs::create_dir(source.join("nested")).unwrap();
    fs::write(source.join("nested/kept"), "kept\n").unwrap();
    fs::write(source.join("nested/deleted"), "deleted\n").unwrap();
    git(&source, &["add", "nested"]);
    git(
        &source,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-m",
            "Add nested files",
        ],
    );
    fs::remove_file(source.join("nested/deleted")).unwrap();
    fs::write(source.join("tracked"), "changed\n").unwrap();
    fs::write(source.join("untracked"), "new\n").unwrap();
    fs::remove_file(source.join(".gitignore")).unwrap();
    let output = fixture.ok(&["adopt", source.to_str().unwrap(), "--repo", repo, "--copy"]);
    assert_eq!(output["branch"], "copy/topic-2");
    let destination = PathBuf::from(output["path"].as_str().unwrap());
    assert!(destination.starts_with(fixture.shoal_dir()));
    assert_eq!(
        fs::read_to_string(destination.join("tracked")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("untracked")).unwrap(),
        "new\n"
    );
    assert!(!destination.join(".gitignore").exists());
    assert_eq!(
        fs::read_to_string(destination.join("nested/kept")).unwrap(),
        "kept\n"
    );
    assert!(!destination.join("nested/deleted").exists());
    assert_eq!(
        fs::read_to_string(source.join("tracked")).unwrap(),
        "changed\n"
    );
    assert!(!source.join(".gitignore").exists());
    assert!(source.exists());
}

#[test]
fn adopt_copy_rolls_back_when_adoption_name_is_taken() {
    let fixture = Fixture::new();
    let source = fixture.root.path().join("external worktree");
    let occupied = fixture.root.path().join("occupied worktree");
    let repo = fixture.repo.to_str().unwrap();
    let occupied_workspace = fixture.ok(&[
        "start",
        repo,
        "copy-topic-2",
        "--path",
        occupied.to_str().unwrap(),
    ]);
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "copy/topic",
            source.to_str().unwrap(),
        ],
    );
    fs::write(source.join("tracked"), "changed in source\n").unwrap();
    fs::write(source.join("untracked"), "preserve source\n").unwrap();
    let worktrees = git(&fixture.repo, &["worktree", "list", "--porcelain"]);
    let repositories = fixture.ok(&["repo", "ls"]);
    let destination =
        Path::new(repositories[0]["workspaces_dir"].as_str().unwrap()).join("copy-topic-2");

    let output = fixture.run(&["adopt", source.to_str().unwrap(), "--repo", repo, "--copy"]);
    assert!(!output.status.success());
    assert!(source.exists());
    assert_eq!(
        fs::read_to_string(source.join("tracked")).unwrap(),
        "changed in source\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("untracked")).unwrap(),
        "preserve source\n"
    );

    assert!(!destination.exists());
    assert!(
        git(&fixture.repo, &["branch", "--list", "copy/topic-2"])
            .trim()
            .is_empty()
    );
    assert_eq!(
        git(&fixture.repo, &["worktree", "list", "--porcelain"]),
        worktrees
    );
    assert_eq!(
        fs::read_to_string(occupied.join("tracked")).unwrap(),
        "committed\n"
    );
    let workspaces = fixture.ok(&["list"]);
    assert_eq!(workspaces.as_array().unwrap().len(), 1);
    assert_eq!(workspaces[0]["id"], occupied_workspace["id"]);
    assert_eq!(workspaces[0]["path"], occupied_workspace["path"]);
}

#[test]
fn adopt_copy_refuses_a_dangling_symlink_at_the_default_directory() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let add = |branch: &str, name: &str| {
        let path = fixture.root.path().join(name);
        git(
            &fixture.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        path
    };
    let first = add("copy/first", "first worktree");
    let output = fixture.ok(&["adopt", first.to_str().unwrap(), "--repo", repo, "--copy"]);
    let directory = PathBuf::from(output["path"].as_str().unwrap());
    let directory = directory.parent().unwrap();
    let source = add("copy/second", "second worktree");
    let planted = directory.join("copy-second-2");
    std::os::unix::fs::symlink(fixture.root.path().join("missing"), &planted).unwrap();
    let output = fixture.run(&["adopt", source.to_str().unwrap(), "--repo", repo, "--copy"]);
    assert!(!output.status.success());
    assert!(planted.is_symlink());
    let branches = git(&fixture.repo, &["branch", "--list", "copy/second-2"]);
    assert!(branches.trim().is_empty());
}

#[test]
fn adopt_copy_refuses_a_default_directory_that_resolves_into_the_source() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    let add = |branch: &str, name: &str| {
        let path = fixture.root.path().join(name);
        git(
            &fixture.repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        fs::canonicalize(path).unwrap()
    };
    let first = add("copy/first", "first worktree");
    let output = fixture.ok(&["adopt", first.to_str().unwrap(), "--repo", repo, "--copy"]);
    let directory = PathBuf::from(output["path"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_owned();
    fixture.ok(&["rm", output["name"].as_str().unwrap(), "--yes"]);
    let source = add("copy/second", "second worktree");
    fs::remove_dir_all(&directory).unwrap();
    std::os::unix::fs::symlink(&source, &directory).unwrap();
    let output = fixture.run(&["adopt", source.to_str().unwrap(), "--repo", repo, "--copy"]);
    assert!(!output.status.success());
    assert!(!source.join("copy-second-2").exists());
}

#[test]
fn adopt_infers_a_checkout_with_a_separate_git_directory() {
    let fixture = Fixture::new();
    let root = fixture.root.path();
    let checkout = root.join("separate");
    fs::create_dir(&checkout).unwrap();
    let checkout = fs::canonicalize(checkout).unwrap();
    let git_dir = root.join("separate.git");
    git(
        &checkout,
        &[
            "init",
            "-b",
            "main",
            "--separate-git-dir",
            git_dir.to_str().unwrap(),
        ],
    );
    git(
        &checkout,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    fixture.ok(&["repo", "add", checkout.to_str().unwrap()]);
    let path = root.join("separate-worktree");
    git(
        &checkout,
        &["worktree", "add", "-b", "topic", path.to_str().unwrap()],
    );
    let adopted = fixture.ok(&["adopt", path.to_str().unwrap()]);
    assert_eq!(adopted["name"], "topic");
}

#[test]
fn adopt_infers_the_repository_from_the_worktree_remote() {
    let fixture = Fixture::new();
    fixture.add_github_origin();
    let adopt = |name: &str, remote: &str| {
        let clone = init_repo(fixture.root.path(), name, &[("tracked", "clone\n")]);
        git(&clone, &["remote", "add", "origin", remote]);
        let path = fixture.root.path().join(format!("{name}-worktree"));
        git(
            &clone,
            &["worktree", "add", "-b", "topic", path.to_str().unwrap()],
        );
        let output = fixture.run(&["adopt", path.to_str().unwrap()]);
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    // The shared remote selects the registration, whose checkout then refuses
    // a worktree of another clone.
    let error = adopt("clone", "git@github.com:team/project.git");
    assert!(
        error.contains("linked worktree of this repository"),
        "{error}"
    );
    let error = adopt("other", "git@github.com:team/other.git");
    assert!(error.contains("pass --repo <repository>"), "{error}");
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
        "start",
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
    fixture.ok(&["adopt", adopted.to_str().unwrap(), "--repo", repo]);
    for protected in [
        offline.join("nested"),
        Path::new(registration["workspaces_dir"].as_str().unwrap()).join("nested"),
    ] {
        assert!(
            !fixture
                .run(&[
                    "start",
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
