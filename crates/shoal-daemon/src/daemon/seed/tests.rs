use super::*;
use crate::daemon::test_support::{
    commit, git, install_test_script, manager, native_cloning, repository,
};
use crate::presets::Preset;
use crate::state::WorkspaceState;
use shoal_protocol::space_names::SpaceSelector;
use std::path::PathBuf;

fn modified(path: &Path) -> std::time::SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

/// Builds wait under host pressure; tests choose the pressure they see.
async fn configure(manager: &Manager, text: &str) {
    crate::daemon::test_support::configure(manager, text).await;
}

const NO_PRESSURE: &str =
    "[machine.overload.memory]\nenabled = false\n[machine.overload.disk]\nenabled = false\n";

/// Without native cloning a seed build fails at its clone and is reported,
/// never copied.
async fn assert_clone_refused(manager: &Manager, layout: &Layout) {
    assert!(Snapshot::open(layout).unwrap().is_none());
    let notifications = manager.notifications.list(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == AlertKind::SeedFailed),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn new_workspaces_start_from_the_default_branch_build() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let checkout = repository(temp.path(), "repo");
    // The build edits a tracked file, which must not lend its time.
    let script = checkout.join("seed.sh");
    install_test_script(
        &script,
        "#!/bin/sh\ntouch \"$GOCACHE/seed\" || exit 1\nmkdir -p out\ncat other > out/built\necho edit >> tracked\n",
    );
    fs::write(
            checkout.join(".shoal.toml"),
            "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n[env]\nGOCACHE = '{cache.go}'\n[caches.go]\nscope = 'shared'\n",
        )
        .unwrap();
    fs::write(checkout.join("other"), "first\n").unwrap();
    for file in ["seed.sh", ".shoal.toml", "other"] {
        commit(&checkout, file);
    }
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager.refresh_seeds().await;
    // The build prepared and used the shared cache.
    let cache = repo.workspaces_dir.join(".caches/shared/go");
    assert!(cache.join("seed").exists());
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&manager, &layout).await;
    }
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    let built = git(&checkout, &["rev-parse", "main"]);
    assert_eq!(snapshot.commit, built.trim());

    fs::write(checkout.join("other"), "second\n").unwrap();
    commit(&checkout, "other");
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(workspace.path.join("out/built")).unwrap(),
        "first\n"
    );
    let seed = layout.worktree();
    assert_eq!(
        modified(&workspace.path.join("seed.sh")),
        modified(&seed.join("seed.sh"))
    );
    for changed in ["other", "tracked"] {
        assert_ne!(
            modified(&workspace.path.join(changed)),
            modified(&seed.join(changed)),
            "{changed}"
        );
    }

    // The moved default branch rebuilds the seed, restoring the edited file.
    manager.refresh_seeds().await;
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert_eq!(
        snapshot.commit,
        git(&checkout, &["rev-parse", "main"]).trim()
    );
    assert_eq!(
        fs::read_to_string(seed.join("tracked")).unwrap(),
        "initial\nedit\n"
    );
    // Shoal's own seed worktree is never listed as untracked.
    let untracked = manager.untracked_worktrees().await.unwrap();
    assert!(untracked.is_empty(), "{untracked:?}");

    manager.remove_repository(&repo.id).await.unwrap();
    assert!(!layout.root().exists());
}

#[tokio::test]
async fn a_failed_build_is_reported_once_per_commit() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let checkout = repository(temp.path(), "repo");
    let script = checkout.join("seed.sh");
    install_test_script(&script, "#!/bin/sh\necho broken\nexit 3\n");
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n",
    )
    .unwrap();
    for file in ["seed.sh", ".shoal.toml"] {
        commit(&checkout, file);
    }
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let layout = Layout::of(&repo);
    // A warning threshold above any free space defers the build.
    configure(
        &manager,
        "[machine.overload.memory]\nenabled = false\n[machine.overload.warning]\ndisk_free_gib = 1000000\n",
    )
    .await;
    manager.refresh_seeds().await;
    assert!(!layout.log().exists(), "the build waits for headroom");
    configure(&manager, NO_PRESSURE).await;
    manager.refresh_seeds().await;
    let log = fs::read_to_string(layout.log()).unwrap();
    assert!(log.contains("broken\n"), "{log}");
    fs::remove_file(layout.log()).unwrap();
    manager.refresh_seeds().await;
    assert!(!layout.log().exists(), "the failed commit is not rebuilt");
    // Other settings at the same commit are a new build.
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n[env]\nMODE = 'seed'\n",
    )
    .unwrap();
    manager.refresh_seeds().await;
    assert!(layout.log().exists(), "changed settings rebuild");
    // Returning to the failed settings retries them.
    fs::remove_file(layout.log()).unwrap();
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n",
    )
    .unwrap();
    manager.refresh_seeds().await;
    assert!(layout.log().exists(), "a revisited failure is retried");
    let notifications = manager.notifications.list(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == AlertKind::SeedFailed),
        "{notifications:?}"
    );
    assert!(Snapshot::open(&layout).unwrap().is_none());
}

#[tokio::test]
async fn a_preset_builds_and_keeps_its_tool_outputs() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let checkout = repository(temp.path(), "repo");
    fs::write(
        checkout.join("Cargo.toml"),
        "[package]\nname = \"seeded\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir(checkout.join("src")).unwrap();
    fs::write(checkout.join("src/lib.rs"), "").unwrap();
    // The runner's own target directory must not take the build.
    fs::write(
        checkout.join(".shoal.toml"),
        "setup.presets = ['cargo']\n[env]\nCARGO_TARGET_DIR = '{workspace_path}/target'\n",
    )
    .unwrap();
    for file in ["Cargo.toml", "src/lib.rs", ".shoal.toml"] {
        commit(&checkout, file);
    }
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    let log = fs::read_to_string(layout.log()).unwrap();
    let built = layout.worktree().join("target/debug/libseeded.rlib");
    assert!(built.is_file(), "{log}");
    // The first build resets what an earlier one could have left.
    assert!(log.starts_with("$ rustc -vV\n"), "{log}");
    assert!(log.contains("$ cargo clean --workspace\n"), "{log}");
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&manager, &layout).await;
    }
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert!(snapshot.toolchains[&Preset::Cargo].starts_with("rustc "));
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    assert!(workspace.path.join("target/debug").is_dir());

    // A change to the code alone builds on the outputs.
    fs::write(checkout.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    commit(&checkout, "src/lib.rs");
    manager.refresh_seeds().await;
    let log = fs::read_to_string(layout.log()).unwrap();
    assert!(!log.contains("cargo clean"), "{log}");
    assert!(log.contains("$ cargo clippy --all-targets\n"), "{log}");
}

/// A repository whose seed runs `presets` with the fake tools of its `bin`:
/// `cargo` records each command in `target` and fails at `clippy` as a
/// toolchain without clippy does, and `npm ci` exits with `npm_exit`.
async fn fake_tools(
    manager: &Manager,
    root: &Path,
    presets: &str,
    npm_exit: i32,
) -> (PathBuf, Repository) {
    let checkout = repository(root, "repo");
    let tools = [
        ("bin/rustc", "#!/bin/sh\necho 'rustc 1.100.0'\n".to_owned()),
        (
            "bin/cargo",
            "#!/bin/sh\ncase \"$1\" in\nclean) ;;\n\
             clippy) echo 'error: no such command: `clippy`' >&2; exit 101 ;;\n\
             *) mkdir -p target && touch \"target/$1\" ;;\nesac\n"
                .to_owned(),
        ),
        ("bin/npm", format!("#!/bin/sh\nexit {npm_exit}\n")),
    ];
    for (path, script) in &tools {
        install_test_script(checkout.join(path), script);
        commit(&checkout, path);
    }
    fs::write(
        checkout.join(".shoal.toml"),
        format!(
            "setup.presets = {presets}\n[env]\nPATH = '{{workspace_path}}/bin:/usr/bin:/bin'\n"
        ),
    )
    .unwrap();
    commit(&checkout, ".shoal.toml");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (checkout, repo)
}

/// The messages of the unread seed failures.
async fn seed_failures(manager: &Manager) -> Vec<String> {
    let notifications = manager.notifications.list(true, 10).await.unwrap();
    (notifications.into_iter())
        .filter(|notification| notification.kind == AlertKind::SeedFailed)
        .map(|notification| notification.message)
        .collect()
}

#[tokio::test]
async fn a_failed_soft_step_keeps_the_other_steps_outputs() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (checkout, repo) = fake_tools(&manager, temp.path(), "['cargo']", 0).await;
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&manager, &layout).await;
    }
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert_eq!(
        snapshot.commit,
        git(&checkout, &["rev-parse", "main"]).trim()
    );
    let failures = seed_failures(&manager).await;
    assert!(
        (failures.iter())
            .any(|message| message.contains("cargo clippy --all-targets exited with 101")),
        "{failures:?}"
    );
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    for built in ["build", "test", "check"] {
        assert!(
            workspace.path.join("target").join(built).is_file(),
            "{built}"
        );
    }
    // The snapshot is current, so the same tip and settings do not rebuild.
    fs::remove_file(layout.log()).unwrap();
    manager.refresh_seeds().await;
    assert!(!layout.log().exists(), "the seed is not rebuilt");
}

#[tokio::test]
async fn a_failure_after_a_soft_one_fails_the_build() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (_, repo) = fake_tools(&manager, temp.path(), "['cargo', 'npm']", 1).await;
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    let log = fs::read_to_string(layout.log()).unwrap();
    assert!(log.contains("$ npm ci\n"), "{log}");
    assert!(Snapshot::open(&layout).unwrap().is_none());
    // The build's failure is the report; the soft one shows only in the log.
    let failures = seed_failures(&manager).await;
    assert!(
        (failures.iter()).any(|message| message.contains("npm ci exited with 1")),
        "{failures:?}"
    );
    assert!(
        !(failures.iter()).any(|message| message.contains("clippy")),
        "{failures:?}"
    );
}

/// A repository whose checkout configures `script` as its seed.
async fn seeded(manager: &Manager, root: &Path, script: &str) -> (PathBuf, Repository) {
    let checkout = repository(root, "repo");
    let path = checkout.join("seed.sh");
    install_test_script(&path, script);
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n",
    )
    .unwrap();
    for file in ["seed.sh", ".shoal.toml"] {
        commit(&checkout, file);
    }
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (checkout, repo)
}

#[tokio::test]
async fn new_workspaces_take_the_spare_which_is_then_refilled() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (_, repo) = seeded(
        &manager,
        temp.path(),
        "#!/bin/sh\nmkdir -p out\necho built > out/built\n",
    )
    .await;
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&manager, &layout).await;
    }
    // The new snapshot asked for its spare.
    tokio::time::timeout(
        Duration::from_secs(60),
        manager.repositories.seeds().spare_wanted(),
    )
    .await
    .expect("the build wants a spare");
    manager.refill_spares().await;
    assert!(layout.has_spare());
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(workspace.path.join("out/built")).unwrap(),
        "built\n"
    );
    assert!(!layout.has_spare());
    tokio::time::timeout(
        Duration::from_secs(60),
        manager.repositories.seeds().spare_wanted(),
    )
    .await
    .expect("taking the spare wants another");
    manager.refill_spares().await;
    assert!(layout.has_spare());
}

#[tokio::test]
async fn repository_removal_cancels_a_running_build() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (_, repo) = seeded(
        &manager,
        temp.path(),
        "#!/bin/sh\ntouch started\nexec sleep 600\n",
    )
    .await;
    let layout = Layout::of(&repo);
    let refresh = tokio::spawn({
        let manager = manager.clone();
        async move { manager.refresh_seeds().await }
    });
    // The command starts only while its build holds the slot.
    let started = layout.worktree().join("started");
    tokio::time::timeout(Duration::from_secs(60), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the build starts");
    manager.remove_repository(&repo.id).await.unwrap();
    assert!(!layout.root().exists());
    tokio::time::timeout(Duration::from_secs(60), refresh)
        .await
        .expect("the cancelled build returns")
        .unwrap();
}

#[tokio::test]
async fn a_removed_seed_is_neither_applied_nor_kept() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (checkout, repo) = seeded(
        &manager,
        temp.path(),
        "#!/bin/sh\nmkdir -p out\ntouch out/built\n",
    )
    .await;
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&manager, &layout).await;
    }
    assert!(Snapshot::open(&layout).unwrap().is_some());
    // Outputs built under other settings are not applied.
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n[env]\nMODE = 'other'\n",
    )
    .unwrap();
    commit(&checkout, ".shoal.toml");
    let changed = manager
        .create_workspace(&repo.id, "changed".into(), None, None, None)
        .await
        .unwrap();
    assert!(!changed.path.join("out").exists());
    git(&checkout, &["rm", "-q", ".shoal.toml"]);
    git(&checkout, &["commit", "-q", "-m", "Drop the seed"]);
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    assert!(!workspace.path.join("out").exists());
    manager.refresh_seeds().await;
    assert!(!layout.root().exists());
}

#[tokio::test]
async fn removal_keeps_a_seed_worktree_shoal_did_not_create() {
    let (temp, manager) = manager().await;
    let (_, repo) = seeded(&manager, temp.path(), "#!/bin/sh\n").await;
    let layout = Layout::of(&repo);
    fs::create_dir_all(layout.worktree()).unwrap();
    fs::write(layout.worktree().join("keep"), "user data").unwrap();
    assert!(manager.remove_repository(&repo.id).await.is_err());
    assert!(layout.worktree().join("keep").exists());
}

#[tokio::test]
async fn processes_a_build_leaves_behind_are_stopped() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (_, repo) = seeded(
        &manager,
        temp.path(),
        "#!/bin/sh\nsleep 600 &\necho $! > ../leftover\nmkdir -p out\n",
    )
    .await;
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    let pid: i32 = fs::read_to_string(layout.root().join("leftover"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // A killed orphan stays a zombie where nothing reaps it, as in a
    // container without an init process; that still counts as stopped.
    let running = || {
        let zombie = fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'))
        });
        // SAFETY: signal 0 only checks that the process exists.
        let exists = unsafe { libc::kill(pid, 0) } == 0;
        exists && !zombie
    };
    tokio::time::timeout(Duration::from_secs(60), async {
        while running() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the leftover process is stopped");
}

#[tokio::test]
async fn removal_keeps_a_symlinked_seed_worktree() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (_, repo) = seeded(&manager, temp.path(), "#!/bin/sh\nmkdir -p out\n").await;
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    if !layout.worktree().exists() {
        return; // The seed was not created on this filesystem.
    }
    // The real worktree moves away and a symlink takes its place.
    let moved = temp.path().join("moved");
    fs::rename(layout.worktree(), &moved).unwrap();
    git(&repo.path, &["worktree", "repair", moved.to_str().unwrap()]);
    std::os::unix::fs::symlink(&moved, layout.worktree()).unwrap();
    assert!(manager.remove_repository(&repo.id).await.is_err());
    assert!(moved.join("seed.sh").exists());
}

#[tokio::test]
async fn a_start_releases_the_git_gate_before_its_seed_step() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (_, repo) = seeded(&manager, temp.path(), "#!/bin/sh\nmkdir -p out\n").await;
    // A swap holds the snapshot, so the start's seed step waits.
    let slot = manager.repositories.seeds().slot(&repo.id);
    let swap = slot.snapshot().clone().write_owned().await;
    let creating = tokio::spawn({
        let manager = manager.clone();
        let id = repo.id.clone();
        async move { (manager.create_workspace(&id, "feature".into(), None, None, None)).await }
    });
    // Creation records the workspace under the Git gate.
    let id = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(workspace) = manager.list_workspaces().await.unwrap().pop() {
                return workspace.id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("creation records the workspace");
    let git = tokio::time::timeout(Duration::from_secs(60), manager.gates.lock_git(&repo.id))
        .await
        .expect("creation releases the Git gate before its seed step");
    drop(git);
    let workspace = manager.workspace_by_id(&id).await.unwrap();
    assert_eq!(workspace.state, WorkspaceState::Preparing);
    crate::test_support::assert_pending(manager.gates.wait_for_creation(&repo.id, &id)).await;
    drop(swap);
    let workspace = creating.await.unwrap().unwrap();
    assert_eq!(workspace.state, WorkspaceState::Ready);
}

#[tokio::test]
async fn a_start_clones_while_a_refill_reads_the_snapshot() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let (_, repo) = seeded(
        &manager,
        temp.path(),
        "#!/bin/sh\nmkdir -p out\necho built > out/built\n",
    )
    .await;
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&manager, &layout).await;
    }
    // A refill reads the snapshot for its whole clone.
    let slot = manager.repositories.seeds().slot(&repo.id);
    let _refill = slot.snapshot().clone().read_owned().await;
    let creating = manager.create_workspace(&repo.id, "feature".into(), None, None, None);
    let workspace = tokio::time::timeout(Duration::from_secs(60), creating)
        .await
        .expect("the start does not wait for the refill")
        .unwrap();
    assert_eq!(
        fs::read_to_string(workspace.path.join("out/built")).unwrap(),
        "built\n"
    );
}

#[tokio::test]
async fn a_copy_s_starting_checkpoint_holds_the_seed_it_started_with() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    // The output is not ignored, so a checkpoint keeps it.
    let (_, repo) = seeded(
        &manager,
        temp.path(),
        "#!/bin/sh\nmkdir -p out\necho built > out/built\n",
    )
    .await;
    let source = manager
        .create_workspace(&repo.id, "source".into(), None, None, None)
        .await
        .unwrap();
    // Taken before the first build, so it holds no output.
    let checkpoint = manager
        .take_checkpoint(&SpaceSelector::id(&source.id), None)
        .await
        .unwrap();
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&manager, &layout).await;
    }
    let request = crate::protocol::CopySource {
        workspace: SpaceSelector::id(&source.id),
        checkpoint: Some(checkpoint.number),
    };
    let copy = manager
        .create_copy(
            (&repo.id).into(),
            &request,
            crate::daemon::workspace::CopyPlace::Branch(None),
            Default::default(),
        )
        .await
        .unwrap();
    assert!(copy.path.join("out/built").is_file());
    assert!(crate::removal::holds_starting_files(&copy).await.unwrap());
}
