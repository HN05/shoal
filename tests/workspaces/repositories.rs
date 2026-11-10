//! Registering, cloning, syncing and removing repositories, the root and
//! clone directories, and repository setup.
use super::chats::{chat_args, install_recording_agent};
#[cfg(target_os = "macos")]
use super::simulators::SIM_CONFIG;
use super::*;

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
    let workspace = fixture.ok(&["start", &url, "cloned"]);
    assert_eq!(
        Path::new(workspace["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        directory
    );
    fixture.ok(&["rm", "cloned"]);
    assert!(clone.is_dir());
    // No workspace name can collide with the clone's directory.
    let main = fixture.ok(&["start", &url, "main"]);
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
            fixture.ok(&["start", "saldoir-server", "feature"]);
        }
    }
    let ambiguous = fixture.run(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(!ambiguous.status.success());
    let (ambiguous, _) = fixture.interactive(&["repo", "rm", "saldoir-server"], "y\n");
    assert!(!ambiguous.status.success());
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
fn repository_selectors_preserve_names_and_reject_colliding_registered_paths() {
    let fixture = Fixture::new();
    let named = fixture.ok(&[
        "repo",
        "rename",
        fixture.repo.to_str().unwrap(),
        "saldoir-server",
    ]);
    let caller = fixture.root.path().join("caller");
    fs::create_dir(&caller).unwrap();
    let unrelated = init_repo(&caller, "saldoir-server", &[("precious", "keep me\n")]);
    git(
        &unrelated,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/unrelated",
        ],
    );
    let command = |args: &[&str]| {
        fixture
            .command()
            .current_dir(&caller)
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        vec!["repo", "add", "saldoir-server"],
        vec!["repo", "config", "saldoir-server"],
        vec![
            "config",
            "set",
            "--repo",
            "saldoir-server",
            "cleanup.done.enabled",
            "false",
        ],
        vec!["start", "saldoir-server", "collision"],
    ] {
        let output = command(&args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        let local =
            || fixture.ok(&["repo", "config", named["id"].as_str().unwrap()])["path"].clone();
        match (args[0], args[1]) {
            ("repo", "add") => assert_eq!(result["id"], named["id"]),
            ("repo", "config") => assert_eq!(result["path"], local()),
            ("config", _) => assert_eq!(result["config"], local()),
            _ => assert_eq!(result["repository_id"], named["id"]),
        }
    }
    let other = fixture.ok(&[
        "repo",
        "add",
        unrelated.to_str().unwrap(),
        "--name",
        "other",
    ]);
    for args in [
        vec!["repo", "rm", "saldoir-server", "--yes"],
        vec!["repo", "rename", "saldoir-server", "renamed"],
    ] {
        let output = command(&args);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(named["id"].as_str().unwrap()), "{error}");
        assert!(error.contains(other["id"].as_str().unwrap()), "{error}");
    }
    fixture.ok(&["repo", "rename", other["id"].as_str().unwrap(), "different"]);
    // A relative path remains usable when its spelling is not a repository name.
    let output = command(&["repo", "config", "./saldoir-server"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        config["path"],
        fixture.ok(&["repo", "config", other["id"].as_str().unwrap()])["path"]
    );
    // Replace the second registration with an unrelated directory to exercise
    // removal by the displayed name.
    fixture.ok(&["repo", "rm", other["id"].as_str().unwrap(), "--yes"]);
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("precious"), "keep me\n").unwrap();
    let output = command(&["repo", "rm", "saldoir-server", "--yes"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.repo.exists());
    assert_eq!(
        fs::read_to_string(unrelated.join("precious")).unwrap(),
        "keep me\n"
    );
}

#[test]
fn unregistered_repository_path_diagnostic_keeps_the_typed_argument() {
    let fixture = Fixture::new();
    let caller = fixture.root.path().join("caller");
    fs::create_dir(&caller).unwrap();
    let path = init_repo(&caller, "saldoir-server", &[("tracked", "unregistered\n")]);
    let output = fixture
        .command()
        .current_dir(&caller)
        .args(["repo", "rm", "saldoir-server", "--yes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(path.to_str().unwrap()), "{error}");
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
    assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 1);
    let workspace = fixture.ok(&["start", "local", "offline"]);
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
    for (root, sources) in [
        ("~/state/worktrees", vec![&url, &outer]),
        ("~/missing/../state/worktrees", vec![&url, &outer]),
        (
            "~/repo with ' quotes & $literal/worktrees",
            vec![&url, &outer],
        ),
        ("~/outer/worktrees", vec![&outer]),
        ("~/missing/../outer/worktrees", vec![&outer]),
    ] {
        fs::write(&config, format!("machine.root_dir = {root:?}\n")).unwrap();
        fixture.restart();
        for source in sources {
            let output = fixture.run(&["repo", "add", source]);
            assert!(!output.status.success(), "{root}: {source} was registered");
        }
        assert!(!fixture.root.path().join(&root[2..]).exists());
        assert!(!fixture.root.path().join("missing").exists());
        assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 1);
    }
    // The already placed repository still creates workspaces in its own directory.
    let workspace = fixture.add("still-works");
    assert!(Path::new(workspace["path"].as_str().unwrap()).starts_with(fixture.shoal_dir()));
}

#[test]
fn root_and_clone_directories_the_daemon_cannot_write_are_refused() {
    let mut fixture = Fixture::new();
    let url = format!("file://{}", fixture.repo.display());
    let blocked = fixture.root.path().join("file");
    fs::write(&blocked, "").unwrap();
    let clone = blocked.join("clone");
    let output = fixture.run(&["repo", "add", &url, "--path", clone.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(&blocked.display().to_string()));
    let config = fixture.root.path().join(".config/shoal/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "machine.root_dir = \"~/file\"\n").unwrap();
    fixture.restart();
    assert!(!fixture.run(&["repo", "add", &url]).status.success());
    assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 1);
}

#[test]
fn root_directory_normalizes_missing_components_before_creation() {
    let fixture = Fixture::with_config(Some("machine.root_dir = \"~/missing/../clones\"\n"));
    let workspace = fixture.add("normalized-root");
    let root = fs::canonicalize(fixture.root.path()).unwrap();
    assert!(Path::new(workspace["path"].as_str().unwrap()).starts_with(root.join("clones")));
    assert!(!root.join("missing").exists());
}

#[test]
fn migrate_moves_workspaces_into_a_saved_root() {
    let fixture = Fixture::new();
    let workspace = fixture.add("moving");
    let old = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    let root = fs::canonicalize(fixture.root.path()).unwrap().join("moved");

    // A process inside is refused even when confirmed.
    let mut inside = Command::new("sleep")
        .arg("60")
        .current_dir(&old)
        .spawn()
        .unwrap();
    let output = fixture.run(&["move", "root", "~/moved", "--yes"]);
    inside.kill().unwrap();
    inside.wait().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(!fixture.run(&["move", "root", "~/moved"]).status.success());
    assert!(old.exists());

    let moved = fixture.ok(&["move", "root", "~/moved", "--yes"]);
    let [moved] = moved.as_array().unwrap().as_slice() else {
        panic!("expected one move: {moved}");
    };
    let directory = Path::new(moved["repository"]["workspaces_dir"].as_str().unwrap());
    assert_eq!(directory.parent(), Some(root.as_path()));
    assert!(!old.exists());
    let config = fs::read_to_string(fixture.root.path().join(".config/shoal/config.toml")).unwrap();
    assert!(
        config.contains(&format!(
            "machine.root_dir = {:?}",
            fixture.root.path().join("moved")
        )),
        "{config}"
    );

    // The moved workspace still runs commands, and new ones go to the new root.
    let path = directory.join(old.file_name().unwrap());
    let output = fixture.run(&[
        "run",
        "--workspace",
        "moving",
        "--",
        "git",
        "status",
        "--short",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(path.join("tracked").exists());
    let next = fixture.add("next");
    assert!(Path::new(next["path"].as_str().unwrap()).starts_with(directory));
    assert_eq!(
        fixture.ok(&["move", "root", "--yes"]),
        serde_json::json!([])
    );
}

/// Migrating stops the agents running in the moving repositories, then
/// restores them in their workspaces' new paths, which it trusts for them.
#[test]
fn migrate_stops_running_agents_and_resumes_them_after_the_move() {
    let fixture = Fixture::with_config(Some(
        "[machine.overload.disk]\nenabled = false\n[agents.claude]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; touch started; while :; do sleep 1; done']\nresume_command = ['sh', '-c', 'trap \"exit 0\" TERM; touch restored; while :; do sleep 1; done']\n",
    ));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let claude_config = fixture.root.path().join(".claude.json");
    fs::write(&claude_config, r#"{"projects": {}}"#).unwrap();
    let workspace = fixture.add("moving");
    let old = PathBuf::from(workspace["path"].as_str().unwrap());
    let agent = fixture
        .command()
        .args(["claude", "moving"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("agent started", || old.join("started").exists());
    // A workspace running only a command keeps it for `shoal resume`.
    let building = fixture.add("building");
    let building = PathBuf::from(building["path"].as_str().unwrap());
    let command = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "building",
            "--",
            "sh",
            "-c",
            "trap 'exit 0' TERM; touch running; while :; do sleep 1; done",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("command started", || building.join("running").exists());
    let trusted = fs::read_to_string(&claude_config).unwrap();
    assert!(
        trusted.contains(old.parent().unwrap().to_str().unwrap()),
        "{trusted}"
    );

    // Without confirmation nothing stops.
    assert!(!fixture.run(&["move", "root", "~/moved"]).status.success());
    let running = fixture.ok(&["status", "moving"]);
    assert!(
        !running["executions"].as_array().unwrap().is_empty(),
        "{running}"
    );

    let output = fixture.run(&["--json", "move", "root", "~/moved", "--yes"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    // The repository stays registered, so its agents' configs stay known.
    assert!(!stderr.contains("warning"), "{stderr}");
    let moved: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(moved.as_array().unwrap().len(), 1, "{moved}");
    assert!(agent.wait_with_output().unwrap().status.success());
    command.wait_with_output().unwrap();
    let resumed = fixture.ok(&["resume", "building"]);
    let argv = &resumed["commands"][0]["commands"][0];
    assert_eq!(argv[0], "sh", "{resumed}");
    let path = fs::canonicalize(fixture.root.path())
        .unwrap()
        .join("moved")
        .join(old.parent().unwrap().file_name().unwrap())
        .join("moving");
    assert!(!old.exists());
    wait_until("agent restored", || path.join("restored").exists());
    // Trusting the repository directory trusts the workspaces in it.
    let trusted: Value =
        serde_json::from_str(&fs::read_to_string(&claude_config).unwrap()).unwrap();
    let directory = path.parent().unwrap().to_str().unwrap();
    assert_eq!(
        trusted["projects"][directory]["hasTrustDialogAccepted"], true,
        "{trusted}"
    );
    let old_directory = old.parent().unwrap().to_str().unwrap();
    assert!(trusted["projects"][old_directory].is_null(), "{trusted}");
    fixture.ok(&["stop", "moving"]);
}

#[test]
fn clone_directories_use_repo_names_and_suffix_occupied_or_recorded_paths() {
    let fixture = Fixture::with_config(Some("machine.root_dir = \"~/clones\"\n"));
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
    let config = format!("machine.root_dir = {:?}", shared.path().to_str().unwrap());
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
    let mut fixture = Fixture::with_config(Some(
        "machine.root_dir = \"~/clones with ' quotes & $literal\"\n",
    ));
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
        format!("machine.root_dir = {:?}\n", new_root.to_str().unwrap()),
    )
    .unwrap();
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    let retained = fixture.ok(&["start", &url, "retained"]);
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
    let fixture = Fixture::with_config(Some("machine.root_dir = \"~/default-clones\"\n"));
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
    assert!(!mismatch.exists());
    assert_eq!(fixture.ok(&["repo", "add", &url]), repo);
    fixture.ok(&["start", &url, "custom-clone"]);
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
    assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 1);
}

#[test]
fn registration_reuses_repositories_by_origin_across_paths_and_url_forms() {
    let fixture = Fixture::new();
    let original = fixture.ok(&["repo", "ls"])[0].clone();
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
    assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 1);
    // This remote tests identity matching only; explicitly use local history.
    fixture.ok(&["start", ssh_url, "alias", "--base", "HEAD"]);
    fixture.ok(&["rm", "alias"]);
}

#[test]
fn the_daemon_alerts_about_a_bare_flag_on_a_checkout_with_a_working_tree() {
    let fixture = Fixture::new();
    fixture.add("owned");
    git(&fixture.repo, &["config", "core.bare", "true"]);
    fixture.ok(&["daemon", "reload"]);
    wait_until("the core.bare alert", || {
        let alerts = fixture.ok(&["alerts", "--all", "--keep-unread"]);
        alerts
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "environment_problem")
            .filter_map(|event| event["message"].as_str())
            .any(|message| message.contains("core.bare=true"))
    });
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
    let second_path = Path::new(second["path"].as_str().unwrap());
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    let workspaces_dir = Path::new(repository["workspaces_dir"].as_str().unwrap());
    let repo_key = fs::canonicalize(&fixture.repo).unwrap();
    let workspaces_key = fs::canonicalize(workspaces_dir).unwrap();
    let first_key = fs::canonicalize(first_path).unwrap();
    let second_key = fs::canonicalize(second_path).unwrap();
    fs::write(
        fixture.root.path().join(".claude.json"),
        serde_json::json!({
            "projects": {
                repo_key.to_str().unwrap(): {"hasTrustDialogAccepted": true},
                workspaces_key.to_str().unwrap(): {"hasTrustDialogAccepted": true},
                first_key.to_str().unwrap(): {"hasTrustDialogAccepted": true},
                second_key.to_str().unwrap(): {"hasTrustDialogAccepted": true},
                "/keep": {"hasTrustDialogAccepted": true}
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::create_dir_all(fixture.root.path().join(".codex")).unwrap();
    fs::write(
        fixture.root.path().join(".codex/config.toml"),
        format!(
            "[projects]\n{} = {{ trust_level = 'trusted' }}\n{} = {{ trust_level = 'trusted' }}\n{} = {{ trust_level = 'trusted' }}\n{} = {{ trust_level = 'trusted' }}\n'/keep' = {{ trust_level = 'trusted' }}\n",
            toml_edit::Key::new(repo_key.to_str().unwrap()),
            toml_edit::Key::new(workspaces_key.to_str().unwrap()),
            toml_edit::Key::new(first_key.to_str().unwrap()),
            toml_edit::Key::new(second_key.to_str().unwrap()),
        ),
    )
    .unwrap();
    fs::write(first_path.join("dirty"), "uncommitted work").unwrap();
    fs::write(first_path.join(".shoal.toml"), "[resources.local-lock]\n").unwrap();
    fs::write(fixture.repo.join("untracked"), "repo changes").unwrap();
    fixture.ok(&["acquire", "port", "web", "--workspace", "first"]);
    fixture.ok(&["acquire", "resource", "local-lock", "--workspace", "first"]);
    fixture.ok(&[
        "acquire",
        "resource",
        "global-lock",
        "--workspace",
        "second",
    ]);
    let declined = fixture.run(&["repo", "rm", "doomed"]);
    assert!(!declined.status.success());
    let scoped = fixture.run(&[
        "run",
        "--workspace",
        "second",
        "--",
        support::shoal_binary(),
        "repo",
        "rm",
        "doomed",
        "--yes",
    ]);
    assert!(!scoped.status.success());
    let mut command = fixture
        .command()
        .args(["run", "--workspace", "first", "--"])
        .args(READABLE_SLEEP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("execution registration", || {
        !fixture.ok(&["status", "first"])["executions"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    let result = fixture.ok(&["repo", "rm", "doomed", "-y"]);
    assert_eq!(result["removed"], true);
    assert_eq!(result["repository_id"], repo["id"]);
    assert_eq!(result["workspaces_removed"], 2);
    assert!(!command.wait().unwrap().success());
    assert!(!fixture.repo.exists());
    assert!(!first_path.exists());
    assert!(!Path::new(second["path"].as_str().unwrap()).exists());
    assert!(fixture.ok(&["repo", "ls"]).as_array().unwrap().is_empty());
    assert!(fixture.ok(&["list"]).as_array().unwrap().is_empty());
    let claude: Value = serde_json::from_str(
        &fs::read_to_string(fixture.root.path().join(".claude.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        claude["projects"],
        serde_json::json!({"/keep": {"hasTrustDialogAccepted": true}})
    );
    let codex: toml::Value = toml::from_str(
        &fs::read_to_string(fixture.root.path().join(".codex/config.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(codex["projects"].as_table().unwrap().len(), 1);
    assert_eq!(
        codex["projects"]["/keep"]["trust_level"].as_str(),
        Some("trusted")
    );
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
    assert!(external.exists() && path.exists());
    git(path, &["worktree", "remove", external.to_str().unwrap()]);
    fs::write(path.join("local-work"), "discarded explicitly").unwrap();
    fixture.ok(&["repo", "rm", &url, "--yes"]);
    assert!(!path.exists());
    assert!(fixture.repo.exists());
    assert_eq!(fixture.ok(&["repo", "ls"]).as_array().unwrap().len(), 1);
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
    fixture.ok(&["repo", "rm", target, "--yes"]);
    assert!(!fixture.repo.exists());
    assert!(fixture.ok(&["repo", "ls"]).as_array().unwrap().is_empty());
}

#[test]
fn repository_removal_confirmation_identifies_the_resolved_checkout() {
    let fixture = Fixture::new();
    let source = fixture.root.path().join("saldoir-server.git");
    git(
        &fixture.repo,
        &["clone", "--bare", ".", source.to_str().unwrap()],
    );
    let url = format!("file://{}", source.display());
    let checkout = fixture.root.path().join("saldera-server/.checkout");
    let repository = fixture.ok(&["repo", "add", &url, "--path", checkout.to_str().unwrap()]);
    let (output, prompt) = fixture.interactive(&["repo", "rm", "saldoir-server"], "n\n");
    assert!(!output.status.success());
    assert!(prompt.contains(&format!("Source: {url}")), "{prompt}");
    assert!(
        prompt.contains(&format!(
            "Checkout: {}",
            repository["path"].as_str().unwrap()
        )),
        "{prompt}"
    );
    assert!(checkout.exists());
    assert!(fixture.repo.exists());
}

#[test]
fn repository_removal_keeps_the_confirmed_target_when_a_name_is_reassigned() {
    use std::io::Read;

    let fixture = Fixture::new();
    let selected = fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    let other = init_repo(fixture.root.path(), "other", &[("tracked", "keep\n")]);
    let replacement = fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    let (mut master, slave) = pty::open();
    let mut child = fixture
        .command()
        .args(["repo", "rm", "project"])
        .stdin(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut transcript = String::new();
    try_wait_for(|| {
        let _ = master.read_to_string(&mut transcript);
        if transcript.contains("Are you sure? [y/N]") {
            return Some(());
        }
        assert!(child.try_wait().unwrap().is_none(), "{transcript}");
        None
    })
    .unwrap_or_else(|| panic!("hung confirmation: {transcript}"));
    fixture.ok(&[
        "repo",
        "rename",
        selected["id"].as_str().unwrap(),
        "renamed",
    ]);
    fixture.ok(&[
        "repo",
        "rename",
        replacement["id"].as_str().unwrap(),
        "project",
    ]);
    master.write_all(b"y\n").unwrap();
    try_wait_for(|| {
        let _ = master.read_to_string(&mut transcript);
        child.try_wait().unwrap()
    })
    .unwrap_or_else(|| panic!("hung removal: {transcript}"));
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{transcript}");
    assert!(!fixture.repo.exists());
    assert!(other.join("tracked").exists());
    let repositories = fixture.ok(&["repo", "ls"]);
    assert!(
        repositories
            .as_array()
            .unwrap()
            .iter()
            .any(|repo| repo["id"] == replacement["id"])
    );
}

#[test]
fn repository_removal_retries_partial_file_deletion_after_restart_but_rejects_replacement() {
    use std::os::unix::fs::MetadataExt;
    let mut fixture = Fixture::new();
    let repo = fixture.ok(&["repo", "ls"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let metadata = fs::metadata(&fixture.repo).unwrap();
    let identity = format!("{}:{}", metadata.dev(), metadata.ino());
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    db.execute("INSERT INTO repository_removals(repository_id,directory_id,deleting_files) VALUES (?1,?2,1)", rusqlite::params![id, identity]).unwrap();
    let refused = fixture.run(&["start", id, "too-late"]);
    assert!(!refused.status.success());
    fs::remove_dir_all(fixture.repo.join(".git")).unwrap();
    fixture.restart();
    let saved = fixture.root.path().join("original-directory");
    fs::rename(&fixture.repo, &saved).unwrap();
    fs::create_dir(&fixture.repo).unwrap();
    fs::write(fixture.repo.join("keep"), "replacement").unwrap();
    let refused = fixture.run(&["repo", "rm", id, "--yes"]);
    assert!(!refused.status.success());
    assert_eq!(
        fs::read_to_string(fixture.repo.join("keep")).unwrap(),
        "replacement"
    );
    fs::remove_dir_all(&fixture.repo).unwrap();
    fs::rename(saved, &fixture.repo).unwrap();
    fixture.ok(&["repo", "rm", id, "--yes"]);
    assert!(!fixture.repo.exists());
    assert!(fixture.ok(&["repo", "ls"]).as_array().unwrap().is_empty());
}

#[test]
fn repository_removal_rejects_symlinks_and_nested_registered_repositories() {
    let fixture = Fixture::new();
    let id = fixture.ok(&["repo", "ls"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let saved = fixture.root.path().join("original");
    fs::rename(&fixture.repo, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, &fixture.repo).unwrap();
    let refused = fixture.run(&["repo", "rm", &id, "--yes"]);
    assert!(!refused.status.success());
    assert!(saved.join("tracked").exists());
    fs::remove_file(&fixture.repo).unwrap();
    fs::rename(saved, &fixture.repo).unwrap();
    let nested = fixture.repo.join("nested");
    fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-b", "main"]);
    fixture.ok(&["repo", "add", nested.to_str().unwrap()]);
    let refused = fixture.run(&["repo", "rm", &id, "--yes"]);
    assert!(!refused.status.success());
    assert!(fixture.repo.exists() && nested.exists());
}

#[test]
fn repository_removal_serializes_with_workspace_creation() {
    // Removal refuses a workspace while setup runs, so this race skips setup.
    let fixture = Fixture::with_config(Some("setup.copy_ignored = false\n"));
    let repo = fixture.ok(&["repo", "ls"])[0].clone();
    let id = repo["id"].as_str().unwrap().to_owned();
    let add = fixture
        .command()
        .args(["start", &id, "racing"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    fixture.ok(&["repo", "rm", &id, "--yes"]);
    let add = add.wait_with_output().unwrap();
    if !add.status.success() {
        assert!(
            String::from_utf8_lossy(&add.stderr).contains("repository"),
            "unexpected add failure: {add:?}"
        );
    }
    assert!(fixture.ok(&["repo", "ls"]).as_array().unwrap().is_empty());
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
    let repo = fixture.ok(&["repo", "ls"])[0].clone();
    let id = repo["id"].as_str().unwrap();
    let input = fixture.root.path().join("local.toml");
    fs::write(&input, "[ports.web]\n").unwrap();
    let saved = fixture.save_local_config(id, &input);
    let workspace = fixture.add("worker");
    let port = fixture.ok(&["acquire", "port", "web", "--workspace", "worker"]);
    let resource = fixture.ok(&["acquire", "resource", "lock", "--workspace", "worker"]);
    fixture.ok(&[
        "acquire",
        "sim",
        "--workspace",
        "worker",
        "--clean",
        "--reason",
        "repository removal test",
    ]);
    fs::write(fixture.root.path().join("sim-fail"), "delete").unwrap();
    assert!(!fixture.run(&["repo", "rm", id, "--yes"]).status.success());
    assert!(fixture.repo.exists());
    assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    assert_eq!(
        fixture.ok(&["leases", "port", "--workspace", "worker"])["reserved"][0],
        port
    );
    assert_eq!(
        fixture.ok(&["leases", "resource", "--workspace", "worker"])["leases"][0],
        resource
    );
    fixture.restart();
    assert_eq!(fixture.ok(&["repo", "config", id]), saved);
    assert!(!fixture.run(&["start", id, "blocked"]).status.success());
    fs::remove_file(fixture.root.path().join("sim-fail")).unwrap();
    fixture.ok(&["repo", "rm", id, "--yes"]);
    assert!(!fixture.repo.exists());
    assert!(!Path::new(saved["path"].as_str().unwrap()).exists());
    assert!(
        fixture.ok(&["leases", "sim", "--all"])["simulators"]
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

#[test]
fn repository_removal_cleans_more_agent_configs_than_fit_in_a_frame() {
    let fixture = Fixture::new();
    let home = fixture.root.path();
    let repository = fixture.ok(&[
        "repo",
        "add",
        fixture.repo.to_str().unwrap(),
        "--name",
        "many",
    ]);
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    // Far more than one 64 KiB protocol frame of kept locations.
    let mut db = rusqlite::Connection::open(home.join("state/state.db")).unwrap();
    db.busy_timeout(Duration::from_secs(60)).unwrap();
    let tx = db.transaction().unwrap();
    for index in 0..2000 {
        let environment = serde_json::json!({
            "CLAUDE_CONFIG_DIR": home.join(format!("claude-{index:04}")),
        });
        tx.execute(
            "INSERT INTO repository_agent_environments(repository_id,environment) VALUES (?1,?2)",
            rusqlite::params![repository["id"].as_str().unwrap(), environment.to_string()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let config = home.join("claude-1999/.claude.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        serde_json::json!({"projects": {
            checkout.to_str().unwrap(): {"hasTrustDialogAccepted": true},
            "/keep": {"hasTrustDialogAccepted": true}
        }})
        .to_string(),
    )
    .unwrap();

    fixture.ok(&["repo", "rm", "many", "--yes"]);
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(
        root["projects"],
        serde_json::json!({"/keep": {"hasTrustDialogAccepted": true}})
    );
    // Cleaned, so the daemon forgets them.
    let kept: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM repository_agent_environments",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 0);
}

#[test]
fn repository_setup_starts_a_setup_agent_on_the_setup_branch() {
    let fixture = Fixture::new();
    install_recording_agent(&fixture, "claude");
    let repo = fixture.repo.to_str().unwrap();
    let setup = || {
        fixture
            .command()
            .args(["repo", "setup", repo, "--agent", "claude", "--here"])
            .output()
            .unwrap()
    };
    let output = setup();
    assert!(output.status.success(), "{output:?}");

    // The agent works in a workspace on the setup branch, in setup mode.
    let workspace = &fixture.ok(&["status", "shoal-setup"])["workspace"];
    assert_eq!(workspace["branch"], "shoal-setup", "{workspace}");
    assert_eq!(workspace["mode"], "setup", "{workspace}");
    // It is told where the local config goes, which lies in the
    // repository directory it may work in.
    let local_config = PathBuf::from(
        fixture.ok(&["repo", "config", repo])["path"]
            .as_str()
            .unwrap(),
    );
    let directory = fs::canonicalize(local_config.parent().unwrap()).unwrap();
    let args = chat_args(&fixture);
    let repository_dir = directory.parent().unwrap();
    assert!(
        args.contains(&format!("--add-dir={}", repository_dir.display())),
        "{args:?}"
    );
    let prompt = args.last().unwrap();
    let config = directory.join("config.toml");
    assert!(prompt.contains(config.to_str().unwrap()), "{prompt}");

    // A later setup reopens the same workspace.
    let output = setup();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.ok(&["list"]).as_array().unwrap().len(), 1);
}

#[test]
fn repository_setup_starts_its_agent_past_max_agents() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 1\n[machine.overload.disk]\nenabled = false\n[agents.waiter]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n",
    ));
    install_recording_agent(&fixture, "claude");
    let repo = fixture.repo.to_str().unwrap();
    let mut first = fixture
        .command()
        .args(["start", repo, "first", "--here", "--agent", "waiter"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the first agent start", || {
        fixture
            .ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "first" && workspace["running"] == true)
            && fixture.ok(&["queue"]) == serde_json::json!([])
    });

    let output = fixture
        .command()
        .args(["repo", "setup", repo, "--agent", "claude", "--here"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // The setup agent ran instead of waiting for the first agent's slot.
    assert!(!chat_args(&fixture).is_empty());
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));

    fixture.ok(&["stop", "first"]);
    first.wait().unwrap();
}

#[test]
fn repository_setup_refuses_named_arguments_before_any_picker() {
    let fixture = Fixture::new();
    let repo = fixture.repo.to_str().unwrap();
    // Outside the checkout, an omitted repository is picked, too.
    for args in [
        &["repo", "setup", "missing-repository"][..],
        &["repo", "setup", repo, "--agent", "missing-agent"],
    ] {
        let (output, rows) = fixture.pick(args, "1", "");
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(rows.is_empty(), "{args:?} opened the picker: {rows}");
    }
}
