#[cfg(target_os = "macos")]
use super::simulators::SIM_CONFIG;
use crate::support::{Fixture, git};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

#[test]
fn url_registration_clones_once_and_supports_workspaces() {
    let fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let repo = fixture.ok(&["repo", "add", &url]);
    assert_ne!(
        repo["path"].as_str().unwrap(),
        fixture.repo.to_str().unwrap()
    );
    // Clones live at `~/shoal/<repo>/.checkout` beside that repository's workspaces.
    let clone = Path::new(repo["path"].as_str().unwrap());
    let directory = clone.parent().unwrap();
    assert_eq!(clone.file_name().unwrap(), ".checkout");
    assert_eq!(repo["workspaces_dir"], directory.to_str().unwrap());
    assert_eq!(directory.parent().unwrap(), fixture.shoal_dir());
    assert!(!fixture.root.path().join("state/repositories").exists());
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    let workspace = fixture.ok(&["add", &url, "cloned"]);
    assert_eq!(
        Path::new(workspace["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        directory
    );
    fixture.ok(&["rm", "cloned"]);
    assert!(clone.is_dir());
    // No workspace name can collide with the clone's directory.
    let main = fixture.ok(&["add", &url, "main"]);
    assert_eq!(main["path"], directory.join("main").to_str().unwrap());
    assert_eq!(main["branch"], "main-2");
}

#[test]
fn url_registration_drops_trailing_slashes_from_the_clone_origin() {
    let fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let repo = fixture.ok(&["repo", "add", &format!("{url}//")]);
    assert_eq!(repo["source"], url);
    let clone = Path::new(repo["path"].as_str().unwrap());
    assert_eq!(git(clone, &["remote", "get-url", "origin"]).trim(), url);
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
}

#[test]
fn displayed_repository_name_resolves_old_uuid_clones_and_rejects_ambiguity() {
    let fixture = Fixture::new();
    let mut clones = Vec::new();
    for parent in ["one", "two"] {
        let source = fixture.root.path().join(parent).join("saldoir-server.git");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        git(
            &fixture.repo,
            &["clone", "--bare", ".", source.to_str().unwrap()],
        );
        let url = format!("file://{}", source.display());
        let destination = fixture.root.path().join(format!("old-uuid-{parent}"));
        clones.push(fixture.ok(&["repo", "add", &url, "--path", destination.to_str().unwrap()]));
        if parent == "one" {
            let listing = fixture.run(&["repo", "list"]);
            assert!(String::from_utf8_lossy(&listing.stdout).contains("saldoir-server  file://"));
            fixture.ok(&["add", "saldoir-server", "feature"]);
        }
    }
    let ambiguous = fixture.run(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("repository name is ambiguous"));
    for repo in &clones {
        assert!(Path::new(repo["path"].as_str().unwrap()).exists());
    }
    // An explicit name wins over a colliding inferred name.
    fixture.ok(&[
        "repo",
        "rename",
        clones[1]["id"].as_str().unwrap(),
        "saldoir-server",
    ]);
    fixture.ok(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(!Path::new(clones[1]["path"].as_str().unwrap()).exists());
    assert!(Path::new(clones[0]["path"].as_str().unwrap()).exists());
    // The remaining inferred name now resolves, deleting its workspace too.
    fixture.ok(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(!Path::new(clones[0]["path"].as_str().unwrap()).exists());
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    assert!(fixture.repo.exists());
}

#[test]
fn local_repository_without_remotes_registers_in_place_and_creates_workspaces() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    assert!(git(&fixture.repo, &["remote"]).is_empty());
    let repo = fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "local",
    ]);
    assert_eq!(repo["path"], fixture.repo.to_str().unwrap());
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
    let workspace = fixture.ok(&["add", "local", "offline"]);
    assert_eq!(
        git(
            Path::new(workspace["path"].as_str().unwrap()),
            &["rev-parse", "HEAD"]
        ),
        git(&fixture.repo, &["rev-parse", "main"])
    );
    // In-place checkouts stay put; only their workspaces gather under `~/shoal/<repo>`.
    assert_eq!(
        Path::new(workspace["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        fixture.shoal_dir().join("repo-with---quotes----literal")
    );
    assert!(!fixture.root.path().join("state/repositories").exists());
    let unused = fixture.root.path().join("unused");
    let output = fixture.run(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--path",
        unused.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("local repositories are registered in place")
    );
    assert!(!unused.exists());
}

#[test]
fn placed_checkouts_adopt_their_directory_but_never_another_checkout() {
    let fixture = Fixture::new();
    let init = |path: &Path| {
        fs::create_dir_all(path).unwrap();
        git(path, &["init", "-b", "main"]);
        fs::write(path.join("file"), "x").unwrap();
        git(path, &["add", "."]);
        git(
            path,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-m",
                "i",
            ],
        );
        fixture.ok(&["repo", "add", path.to_str().unwrap()])
    };
    let shoal = fixture.root.path().join("shoal");
    // `~/shoal/placed/main` keeps `~/shoal/placed` for its workspaces.
    let placed = init(&shoal.join("placed/main"));
    assert_eq!(
        placed["workspaces_dir"],
        fixture.shoal_dir().join("placed").to_str().unwrap()
    );
    // A checkout directly under the root is not a repository directory...
    let outer = init(&shoal.join("outer"));
    assert_ne!(
        outer["workspaces_dir"],
        fixture.shoal_dir().to_str().unwrap()
    );
    // ...and a checkout nested in it must not put its workspaces inside `outer`.
    let inner = init(&shoal.join("outer/inner"));
    let inner_dir = Path::new(inner["workspaces_dir"].as_str().unwrap());
    assert_ne!(inner_dir, fixture.shoal_dir().join("outer"));
    assert!(inner_dir.starts_with(fixture.shoal_dir()));
    assert!(!inner_dir.starts_with(fixture.shoal_dir().join("outer")));
}

#[test]
fn root_directory_inside_state_or_a_checkout_is_refused() {
    let mut fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let config = fixture.root.path().join(".config/shoal/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let outer = fixture.root.path().join("outer");
    fs::create_dir(&outer).unwrap();
    git(&outer, &["init", "-b", "main"]);
    let outer = outer.display().to_string();
    // A root inside the state directory or a registered checkout refuses every
    // registration; one inside the checkout being registered refuses that one.
    for (root, message, sources) in [
        ("~/state/worktrees", "state directory", vec![&url, &outer]),
        (
            "~/missing/../state/worktrees",
            "state directory",
            vec![&url, &outer],
        ),
        (
            "~/repo with ' quotes & $literal/worktrees",
            "repository checkout",
            vec![&url, &outer],
        ),
        ("~/outer/worktrees", "repository checkout", vec![&outer]),
        (
            "~/missing/../outer/worktrees",
            "repository checkout",
            vec![&outer],
        ),
    ] {
        fs::write(&config, format!("root_dir = {root:?}\n")).unwrap();
        fixture.restart();
        for source in sources {
            let output = fixture.run(&["repo", "add", source]);
            assert!(!output.status.success(), "{root}: {source} was registered");
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        }
        assert!(!fixture.root.path().join(&root[2..]).exists());
        assert!(!fixture.root.path().join("missing").exists());
        assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
    }
    // The already placed repository still creates workspaces in its own directory.
    let workspace = fixture.add("still-works");
    assert!(Path::new(workspace["path"].as_str().unwrap()).starts_with(fixture.shoal_dir()));
}

#[test]
fn root_directory_normalizes_missing_components_before_creation() {
    let fixture = Fixture::with_config(Some("root_dir = \"~/missing/../clones\"\n"));
    let workspace = fixture.add("normalized-root");
    let root = fs::canonicalize(fixture.root.path()).unwrap();
    assert!(Path::new(workspace["path"].as_str().unwrap()).starts_with(root.join("clones")));
    assert!(!root.join("missing").exists());
}

#[test]
fn clone_directories_use_repo_names_and_suffix_occupied_or_recorded_paths() {
    let fixture = Fixture::with_config(Some("root_dir = \"~/clones\"\n"));
    let directory = fixture.root.path().join("clones");
    fs::write(directory.join("project"), "keep this file").unwrap();
    std::os::unix::fs::symlink("missing-target", directory.join("project-2")).unwrap();
    let mut repos = Vec::new();
    for index in 0..3 {
        let parent = fixture.root.path().join(format!("source-{index}"));
        fs::create_dir(&parent).unwrap();
        let source = parent.join("project.git");
        git(
            &fixture.repo,
            &["clone", "--bare", ".", source.to_str().unwrap()],
        );
        let url = format!("file://{}", source.display());
        let repo = fixture.ok(&["repo", "add", &url]);
        let clone = Path::new(repo["path"].as_str().unwrap());
        assert_eq!(clone.file_name().unwrap(), ".checkout");
        assert_eq!(
            clone
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            format!("project-{}", index + 3)
        );
        assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
        repos.push(repo);
        if index == 0 {
            // A lost directory must retain its path reservation in Shoal's registry.
            fs::remove_dir_all(repos[0]["workspaces_dir"].as_str().unwrap()).unwrap();
        }
    }
    assert_eq!(
        fs::read_to_string(directory.join("project")).unwrap(),
        "keep this file"
    );
    assert_eq!(
        fs::read_link(directory.join("project-2")).unwrap(),
        PathBuf::from("missing-target")
    );
    assert!(!directory.join("project-3").exists());
    let url = format!("file://{}", fixture.repo.display());
    let named = fixture.ok(&["repo", "add", &url, "--name", "chosen"]);
    assert_eq!(
        Path::new(named["path"].as_str().unwrap())
            .parent()
            .unwrap()
            .file_name()
            .unwrap(),
        "chosen"
    );
    let renamed = fixture.ok(&["repo", "rename", "chosen", "new-label"]);
    assert_eq!(renamed["path"], named["path"]);
    assert_eq!(renamed["workspaces_dir"], named["workspaces_dir"]);
}

#[test]
fn clone_name_allocation_is_atomic_across_daemons_sharing_a_directory() {
    let shared = tempfile::tempdir_in("/tmp").unwrap();
    let config = format!("root_dir = {:?}", shared.path().to_str().unwrap());
    let first = Fixture::with_config(Some(&config));
    let second = Fixture::with_config(Some(&config));
    let clone = |fixture: &Fixture| {
        let source = fixture.root.path().join("project.git");
        git(
            &fixture.repo,
            &["clone", "--bare", ".", source.to_str().unwrap()],
        );
        fixture.ok(&["repo", "add", &format!("file://{}", source.display())])
    };
    let (one, two) = thread::scope(|scope| {
        let one = scope.spawn(|| clone(&first));
        let two = scope.spawn(|| clone(&second));
        (one.join().unwrap(), two.join().unwrap())
    });
    let mut names = [one, two].map(|repo| {
        Path::new(repo["workspaces_dir"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    });
    names.sort();
    assert_eq!(names, ["project", "project-2"]);
    assert!(shared.path().join("project/.checkout/.git").exists());
    assert!(shared.path().join("project-2/.checkout/.git").exists());
}

#[test]
fn configured_root_directory_affects_new_repositories_and_preserves_existing_paths() {
    let mut fixture =
        Fixture::with_config(Some("root_dir = \"~/clones with ' quotes & $literal\"\n"));
    let url = format!("file://{}", fixture.repo.display());
    let repo = fixture.ok(&["repo", "add", &url]);
    let path = Path::new(repo["path"].as_str().unwrap());
    assert_eq!(
        path.parent().unwrap().parent().unwrap(),
        fs::canonicalize(fixture.root.path().join("clones with ' quotes & $literal")).unwrap()
    );
    assert!(!fixture.root.path().join("state/repositories").exists());
    let new_root = fixture.root.path().join("new clones");
    fs::write(
        fixture.root.path().join(".config/shoal/config.toml"),
        format!("root_dir = {:?}\n", new_root.to_str().unwrap()),
    )
    .unwrap();
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    let retained = fixture.ok(&["add", &url, "retained"]);
    assert_eq!(
        Path::new(retained["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        path.parent().unwrap()
    );
    assert!(!new_root.exists());
    let source = fixture.root.path().join("second.git");
    git(
        &fixture.repo,
        &["clone", "--bare", ".", source.to_str().unwrap()],
    );
    let second = fixture.ok(&["repo", "add", &format!("file://{}", source.display())]);
    assert_eq!(
        Path::new(second["path"].as_str().unwrap())
            .parent()
            .unwrap()
            .parent()
            .unwrap(),
        fs::canonicalize(new_root).unwrap()
    );
}

#[test]
fn repository_clone_path_overrides_default_and_resolves_in_callers_directory() {
    let fixture = Fixture::with_config(Some("root_dir = \"~/default-clones\"\n"));
    let url = format!("file://{}", fixture.repo.display());
    let relative = "projects/a repo with ' quotes & $literal";
    let output = fixture
        .command()
        .current_dir(fixture.root.path())
        .args(["--json", "repo", "add", &url, "--path", relative])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo: Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected = fs::canonicalize(fixture.root.path().join(relative)).unwrap();
    assert_eq!(repo["path"], expected.to_str().unwrap());
    // The repository directory is still reserved for its workspaces.
    let directory = Path::new(repo["workspaces_dir"].as_str().unwrap());
    assert_eq!(
        directory.parent().unwrap(),
        fs::canonicalize(fixture.root.path().join("default-clones")).unwrap()
    );
    assert!(fs::read_dir(directory).unwrap().next().is_none());
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    assert_eq!(
        fixture.ok(&["repo", "add", &url, "--path", &format!("~/{relative}")]),
        repo
    );
    let mismatch = fixture.root.path().join("other");
    let output = fixture.run(&[
        "repo",
        "add",
        &url,
        "--path",
        mismatch.to_str().unwrap(),
        "--name",
        "wrong",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot relocate"));
    assert!(!mismatch.exists());
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    fixture.ok(&["add", &url, "custom-clone"]);
}

#[test]
fn clone_path_preserves_existing_destinations_and_cleans_only_failed_new_clones() {
    let fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let occupied = fixture.root.path().join("occupied");
    fs::create_dir(&occupied).unwrap();
    let file = occupied.join("keep");
    fs::write(&file, "user data").unwrap();
    let empty = fixture.root.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let link = fixture.root.path().join("link");
    std::os::unix::fs::symlink(&occupied, &link).unwrap();
    for destination in [&occupied, &empty, &file, &link] {
        let output = fixture.run(&["repo", "add", &url, "--path", destination.to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("destination must not already exist")
        );
        assert!(destination.exists());
        assert_eq!(fs::read_to_string(&file).unwrap(), "user data");
    }
    let failed = fixture.root.path().join("failed clone");
    let missing = format!(
        "file://{}",
        fixture.root.path().join("missing.git").display()
    );
    assert!(
        !fixture
            .run(&["repo", "add", &missing, "--path", failed.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!failed.exists());
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn registration_reuses_repositories_by_origin_across_paths_and_url_forms() {
    let fixture = Fixture::new();
    let original = fixture.ok(&["repo", "list"])[0].clone();
    let url = "https://example.invalid/team/project.git";
    let ssh_url = "git@example.invalid:team/project.git";
    git(&fixture.repo, &["remote", "add", "origin", url]);
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let other = fixture.root.path().join("other-checkout");
    git(
        &fixture.repo,
        &["clone", "--local", ".", other.to_str().unwrap()],
    );
    git(&other, &["remote", "set-url", "origin", ssh_url]);
    assert_eq!(
        fixture.ok(&["repo", "add", other.to_str().unwrap()]),
        original
    );
    assert_eq!(fixture.ok(&["repo", "add", ssh_url]), original);
    assert_eq!(fixture.ok(&["repo", "add", url]), original);
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
    // This remote tests identity matching only; explicitly use local history.
    fixture.ok(&["add", ssh_url, "alias", "--base", "HEAD"]);
    fixture.ok(&["rm", "alias"]);
}

#[test]
fn repositories_can_be_named_when_added_and_renamed_without_duplication() {
    let fixture = Fixture::new();
    let original = fixture.ok(&["repo", "list"])[0]["id"].clone();
    let named = fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "project",
    ]);
    assert_eq!(named["id"], original);
    assert_eq!(named["name"], "project");
    fixture.ok(&["add", "project", "named"]);
    let renamed = fixture.ok(&["repo", "rename", "project", "renamed"]);
    assert_eq!(renamed["id"], original);
    assert_eq!(renamed["name"], "renamed");
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
    fixture.ok(&["rm", "named"]);
}

#[test]
fn registry_survives_daemon_restart() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("persistent");
    fixture.run(&["daemon", "stop"]);
    fixture.daemon.wait().unwrap();
    fixture.daemon = fixture
        .command()
        .args(["daemon", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.wait_ready();
    assert_eq!(
        fixture.ok(&["inspect", "persistent"])["workspace"]["id"],
        workspace["id"]
    );
    fixture.ok(&["rm", "persistent"]);
}

#[test]
fn repository_removal_deletes_local_checkout_workspaces_and_leases_and_stops_commands() {
    let fixture = Fixture::with_config(Some("[resources.global-lock]\n"));
    let repo = fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "doomed",
    ]);
    let first = fixture.add("first");
    let second = fixture.add("second");
    let first_path = Path::new(first["path"].as_str().unwrap());
    fs::write(first_path.join("dirty"), "uncommitted work").unwrap();
    fs::write(first_path.join(".shoal.toml"), "[resources.local-lock]\n").unwrap();
    fs::write(fixture.repo.join("untracked"), "repo changes").unwrap();
    fixture.ok(&["port", "acquire", "web", "first"]);
    fixture.ok(&["resource", "acquire", "local-lock", "first"]);
    fixture.ok(&["resource", "acquire", "global-lock", "second"]);
    let declined = fixture.run(&["repo", "rm", "doomed"]);
    assert!(!declined.status.success());
    assert!(String::from_utf8_lossy(&declined.stderr).contains("pass --yes"));
    let scoped = fixture.run(&[
        "exec",
        "second",
        "--",
        env!("CARGO_BIN_EXE_shoal"),
        "repo",
        "rm",
        "doomed",
        "--yes",
    ]);
    assert!(!scoped.status.success());
    assert!(String::from_utf8_lossy(&scoped.stderr).contains("workspace processes can only"));
    let mut command = fixture
        .command()
        .args(["exec", "first", "--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.ok(&["inspect", "first"])["executions"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let result = fixture.ok(&["repo", "rm", "doomed", "-y"]);
    assert_eq!(result["removed"], true);
    assert_eq!(result["repository_id"], repo["id"]);
    assert_eq!(result["workspaces_removed"], 2);
    assert!(!command.wait().unwrap().success());
    assert!(!fixture.repo.exists());
    assert!(!first_path.exists());
    assert!(!Path::new(second["path"].as_str().unwrap()).exists());
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    for table in [
        "ports",
        "resource_leases",
        "executions",
        "repository_removals",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM resource_pools WHERE scope<>'global'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM resource_pools WHERE scope='global'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn repository_removal_preserves_external_worktrees_and_deletes_clone_on_retry() {
    let fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let repo = fixture.ok(&["repo", "add", &url, "--name", "cloned"]);
    let path = Path::new(repo["path"].as_str().unwrap());
    let external = fixture.root.path().join("external");
    git(
        path,
        &[
            "worktree",
            "add",
            "-b",
            "external",
            external.to_str().unwrap(),
        ],
    );
    let refused = fixture.run(&["repo", "rm", "cloned", "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("worktree outside Shoal"));
    assert!(external.exists() && path.exists());
    git(path, &["worktree", "remove", external.to_str().unwrap()]);
    fs::write(path.join("local-work"), "discarded explicitly").unwrap();
    fixture.ok(&["repo", "remove", &url, "--yes"]);
    assert!(!path.exists());
    assert!(fixture.repo.exists());
    assert_eq!(fixture.ok(&["repo", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn repository_removal_ignores_only_missing_prunable_unlocked_worktrees() {
    let fixture = Fixture::new();
    let external = fixture.root.path().join("external with spaces");
    let target = fixture.repo.to_str().unwrap();
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-b",
            "external",
            external.to_str().unwrap(),
        ],
    );
    git(
        &fixture.repo,
        &["worktree", "lock", external.to_str().unwrap()],
    );
    fs::remove_dir_all(&external).unwrap();
    let refused = fixture.run(&["repo", "rm", target, "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("worktree outside Shoal"));
    git(
        &fixture.repo,
        &["worktree", "unlock", external.to_str().unwrap()],
    );
    std::os::unix::fs::symlink(fixture.root.path().join("missing"), &external).unwrap();
    assert!(
        !fixture
            .run(&["repo", "rm", target, "--yes"])
            .status
            .success()
    );
    fs::remove_file(&external).unwrap();
    assert!(git(&fixture.repo, &["worktree", "list", "--porcelain"]).contains("prunable"));
    fixture.ok(&["repo", "rm", target, "--yes"]);
    assert!(!fixture.repo.exists());
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
}

#[test]
fn repository_removal_retries_partial_file_deletion_after_restart_but_rejects_replacement() {
    use std::os::unix::fs::MetadataExt;
    let mut fixture = Fixture::new();
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let metadata = fs::metadata(&fixture.repo).unwrap();
    let identity = format!("{}:{}", metadata.dev(), metadata.ino());
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute("INSERT INTO repository_removals(repository_id,directory_id,deleting_files) VALUES (?1,?2,1)", rusqlite::params![id, identity]).unwrap();
    let refused = fixture.run(&["add", id, "too-late"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("removal is incomplete"));
    fs::remove_dir_all(fixture.repo.join(".git")).unwrap();
    fixture.restart();
    let saved = fixture.root.path().join("original-directory");
    fs::rename(&fixture.repo, &saved).unwrap();
    fs::create_dir(&fixture.repo).unwrap();
    fs::write(fixture.repo.join("keep"), "replacement").unwrap();
    let refused = fixture.run(&["repo", "rm", id, "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("was replaced"));
    assert_eq!(
        fs::read_to_string(fixture.repo.join("keep")).unwrap(),
        "replacement"
    );
    fs::remove_dir_all(&fixture.repo).unwrap();
    fs::rename(saved, &fixture.repo).unwrap();
    fixture.ok(&["repo", "rm", id, "--yes"]);
    assert!(!fixture.repo.exists());
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
}

#[test]
fn repository_removal_rejects_symlinks_and_nested_registered_repositories() {
    let fixture = Fixture::new();
    let id = fixture.ok(&["repo", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let saved = fixture.root.path().join("original");
    fs::rename(&fixture.repo, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, &fixture.repo).unwrap();
    let refused = fixture.run(&["repo", "rm", &id, "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("symlink"));
    assert!(saved.join("tracked").exists());
    fs::remove_file(&fixture.repo).unwrap();
    fs::rename(saved, &fixture.repo).unwrap();
    let nested = fixture.repo.join("nested");
    fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-b", "main"]);
    fixture.ok(&["repo", "add", nested.to_str().unwrap()]);
    let refused = fixture.run(&["repo", "rm", &id, "--yes"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("another registered repository"));
    assert!(fixture.repo.exists() && nested.exists());
}

#[test]
fn repository_removal_serializes_with_workspace_creation() {
    let fixture = Fixture::new();
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap().to_owned();
    let add = fixture
        .command()
        .args(["add", &id, "racing"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    fixture.ok(&["repo", "rm", &id, "--yes"]);
    let _ = add.wait_with_output().unwrap();
    assert!(fixture.ok(&["repo", "list"]).as_array().unwrap().is_empty());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
    // The emptied repository directory goes with the registration.
    assert!(!Path::new(repo["workspaces_dir"].as_str().unwrap()).exists());
    assert!(!fixture.repo.exists());
}

#[test]
#[cfg(target_os = "macos")]
fn repository_removal_preserves_resources_on_failure_and_retries_after_restart() {
    let config = format!("{SIM_CONFIG}\n[resources.lock]\n");
    let mut fixture = Fixture::with_tools(Some(&config), true);
    let repo = fixture.ok(&["repo", "list"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let input = fixture.root.path().join("local.toml");
    fs::write(&input, "[ports.web]\n").unwrap();
    let saved = fixture.ok(&["repo", "config", id, "--file", input.to_str().unwrap()]);
    let workspace = fixture.add("worker");
    let port = fixture.ok(&["port", "acquire", "web", "worker"]);
    let resource = fixture.ok(&["resource", "acquire", "lock", "worker"]);
    fixture.ok(&[
        "sim",
        "acquire",
        "worker",
        "--clean",
        "--reason",
        "repository removal test",
    ]);
    fs::write(fixture.root.path().join("sim-fail"), "delete").unwrap();
    assert!(!fixture.run(&["repo", "rm", id, "--yes"]).status.success());
    assert!(fixture.repo.exists());
    assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    assert_eq!(fixture.ok(&["port", "worker"])["reserved"][0], port);
    assert_eq!(fixture.ok(&["resource", "worker"])["leases"][0], resource);
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    let blocked = fixture.run(&["repo", "config", id, "--clear"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("removal is incomplete"));
    assert!(!fixture.run(&["add", id, "blocked"]).status.success());
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&["repo", "rm", id, "--yes"]);
    assert!(!fixture.repo.exists());
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM repository_configs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        fixture.ok(&["sim", "--all"])["simulators"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("sim-devices.json")).unwrap(),
        "[]"
    );
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    assert!(
        db.query_row("SELECT COUNT(*) FROM simulator_clean_requests", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap()
            > 0
    );
}
