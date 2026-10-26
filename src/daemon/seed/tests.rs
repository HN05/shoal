use super::*;
use crate::test_support::{commit, git, manager, repository};
use std::{os::unix::fs::PermissionsExt as _, path::PathBuf};

fn modified(path: &Path) -> std::time::SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

/// Builds wait under host pressure; tests choose the pressure they see.
async fn configure(manager: &Manager, text: &str) {
    crate::test_support::configure(manager, text).await;
}

const NO_PRESSURE: &str = "[overload.memory]\nenabled = false\n[overload.disk]\nenabled = false\n";

#[tokio::test]
async fn new_workspaces_start_from_the_default_branch_build() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let checkout = repository(temp.path(), "repo");
    // The build edits a tracked file, which must not lend its time.
    let script = checkout.join("seed.sh");
    fs::write(
            &script,
            "#!/bin/sh\ntouch \"$GOCACHE/seed\" || exit 1\nmkdir -p out\ncat other > out/built\necho edit >> tracked\n",
        )
        .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
            checkout.join(".shoal.toml"),
            "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n[env]\nGOCACHE = '{cache.go}'\n[caches.go]\nscope = 'shared'\n",
        )
        .unwrap();
    fs::write(checkout.join("other"), "first\n").unwrap();
    for file in ["seed.sh", ".shoal.toml", "other"] {
        commit(&checkout, file);
    }
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager.refresh_seeds().await;
    // The build prepared and used the shared cache.
    let cache = repo
        .workspaces_dir
        .as_ref()
        .unwrap()
        .join(".caches/shared/go");
    assert!(cache.join("seed").exists());
    let layout = Layout::of(&repo).unwrap();
    let Some(snapshot) = Snapshot::open(&layout).unwrap() else {
        // Without copy-on-write cloning the seed is reported, never copied.
        let notifications = manager.notifications(true, 10).await.unwrap();
        assert!(
            notifications
                .iter()
                .any(|notification| notification.kind == NotificationKind::SeedFailed),
            "{notifications:?}"
        );
        return;
    };
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
    let checks = manager.diagnose().await.unwrap();
    assert!(
        !checks
            .iter()
            .any(|check| check.message.contains("not tracked by Shoal")),
        "{checks:?}"
    );

    manager.remove_repository(&repo.id).await.unwrap();
    assert!(!layout.root().exists());
}

#[tokio::test]
async fn a_failed_build_is_reported_once_per_commit() {
    let (temp, manager) = manager().await;
    configure(&manager, NO_PRESSURE).await;
    let checkout = repository(temp.path(), "repo");
    let script = checkout.join("seed.sh");
    fs::write(&script, "#!/bin/sh\necho broken\nexit 3\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        checkout.join(".shoal.toml"),
        "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n",
    )
    .unwrap();
    for file in ["seed.sh", ".shoal.toml"] {
        commit(&checkout, file);
    }
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let layout = Layout::of(&repo).unwrap();
    // A warning threshold above any free space defers the build.
    configure(
        &manager,
        "[overload.memory]\nenabled = false\n[overload.warning]\ndisk_free_gib = 1000000\n",
    )
    .await;
    manager.refresh_seeds().await;
    assert!(!layout.log().exists(), "the build waits for headroom");
    configure(&manager, NO_PRESSURE).await;
    manager.refresh_seeds().await;
    assert_eq!(
        fs::read_to_string(layout.log()).unwrap(),
        "$ seed.cmd\nbroken\n"
    );
    fs::remove_file(layout.log()).unwrap();
    manager.refresh_seeds().await;
    assert!(!layout.log().exists(), "the failed commit is not rebuilt");
    // Other settings at the same commit are a new build.
    fs::write(
        checkout.join(".shoal.toml"),
        "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n[env]\nMODE = 'seed'\n",
    )
    .unwrap();
    manager.refresh_seeds().await;
    assert!(layout.log().exists(), "changed settings rebuild");
    // Returning to the failed settings retries them.
    fs::remove_file(layout.log()).unwrap();
    fs::write(
        checkout.join(".shoal.toml"),
        "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n",
    )
    .unwrap();
    manager.refresh_seeds().await;
    assert!(layout.log().exists(), "a revisited failure is retried");
    let notifications = manager.notifications(true, 10).await.unwrap();
    let failure = notifications
        .iter()
        .find(|notification| notification.kind == NotificationKind::SeedFailed)
        .unwrap();
    assert!(failure.message.contains("exited with 3"), "{failure:?}");
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
        "presets = ['cargo']\n[env]\nCARGO_TARGET_DIR = '{workspace_path}/target'\n",
    )
    .unwrap();
    for file in ["Cargo.toml", "src/lib.rs", ".shoal.toml"] {
        commit(&checkout, file);
    }
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager.refresh_seeds().await;
    let layout = Layout::of(&repo).unwrap();
    let log = fs::read_to_string(layout.log()).unwrap();
    assert!(log.starts_with("$ cargo build --all-targets\n"), "{log}");
    let built = layout.worktree().join("target/debug/libseeded.rlib");
    assert!(built.is_file(), "{log}");
    // Without copy-on-write cloning the seed is never copied.
    let probe = layout.root().join("probe");
    fs::write(&probe, "x").unwrap();
    let cloning = crate::fsutil::clone_tree(&probe, &layout.root().join("probe-clone"))
        .await
        .is_ok();
    if !cloning {
        return;
    }
    assert!(Snapshot::open(&layout).unwrap().is_some(), "{log}");
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    assert!(workspace.path.join("target/debug").is_dir());
}

/// A repository whose checkout configures `script` as its seed.
async fn seeded(manager: &Manager, root: &Path, script: &str) -> (PathBuf, Repository) {
    let checkout = repository(root, "repo");
    let path = checkout.join("seed.sh");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        checkout.join(".shoal.toml"),
        "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n",
    )
    .unwrap();
    for file in ["seed.sh", ".shoal.toml"] {
        commit(&checkout, file);
    }
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (checkout, repo)
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
    let layout = Layout::of(&repo).unwrap();
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
    let layout = Layout::of(&repo).unwrap();
    if Snapshot::open(&layout).unwrap().is_none() {
        return; // No copy-on-write cloning here; covered above.
    }
    // Outputs built under other settings are not applied.
    fs::write(
        checkout.join(".shoal.toml"),
        "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n[env]\nMODE = 'other'\n",
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
    let layout = Layout::of(&repo).unwrap();
    fs::create_dir_all(layout.worktree()).unwrap();
    fs::write(layout.worktree().join("keep"), "user data").unwrap();
    let checks = manager.diagnose().await.unwrap();
    assert!(
        checks.iter().any(|check| check
            .message
            .contains("Seed path is not Shoal's seed worktree")),
        "{checks:?}"
    );
    let error = manager.remove_repository(&repo.id).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("not the seed worktree"),
        "{error:#}"
    );
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
    let layout = Layout::of(&repo).unwrap();
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
    let layout = Layout::of(&repo).unwrap();
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

#[test]
fn group_membership_reads_the_group_field() {
    // pid 40, command "a) b", state S, parent 7, group 30.
    let stat = "40 (a) b) S 7 30 30 0 -1";
    assert!(live_member(stat, 30));
    assert!(!live_member(stat, 7), "the parent PID is not the group");
    assert!(
        !live_member("40 (a) Z 7 30 30 0 -1", 30),
        "zombies are gone"
    );
}
