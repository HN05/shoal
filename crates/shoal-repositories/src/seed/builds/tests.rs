use std::{fs, path::PathBuf, time::Duration};

use shoal_core::{
    presets::Preset,
    test_support::{commit, git, install_test_script, native_cloning, repository},
};
use shoal_protocol::{
    alerts::AlertKind,
    model::{Repository, Workspace},
};

use crate::{
    seed::{Layout, Snapshot},
    stand_in::Fixture,
};

/// Record the workspace `name` of `repo` and start it from the seed build,
/// as its creation does.
async fn start(fixture: &Fixture, repo: &Repository, name: &str) -> Workspace {
    let workspace = fixture.record_workspace(repo, name, None).await;
    let (repositories, daemon) = (&fixture.repositories, &fixture.daemon);
    let settings = repositories.workspace_settings(daemon, &workspace).await;
    (repositories.apply_seed(daemon, repo, &workspace, &settings.unwrap())).await;
    workspace
}

async fn refresh(fixture: &Fixture) {
    fixture.repositories.refresh_seeds(&fixture.daemon).await;
}

/// The messages of the unread seed failures.
async fn seed_failures(fixture: &Fixture) -> Vec<String> {
    let notifications = fixture.repositories.notifications.list(true, 10).await;
    (notifications.unwrap().into_iter())
        .filter(|notification| notification.kind == AlertKind::SeedFailed)
        .map(|notification| notification.message)
        .collect()
}

/// Without native cloning a seed build fails at its clone and is reported,
/// never copied.
async fn assert_clone_refused(fixture: &Fixture, layout: &Layout) {
    assert!(Snapshot::open(layout).unwrap().is_none());
    let failures = seed_failures(fixture).await;
    assert!(!failures.is_empty(), "{failures:?}");
}

/// A repository whose checkout configures `script` as its seed.
async fn seeded(fixture: &Fixture, script: &str) -> (PathBuf, Repository) {
    let checkout = repository(fixture.root.path(), "repo");
    install_test_script(checkout.join("seed.sh"), script);
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n",
    )
    .unwrap();
    for file in ["seed.sh", ".shoal.toml"] {
        commit(&checkout, file);
    }
    let repo = fixture.register(checkout.display()).await;
    (checkout, repo)
}

#[tokio::test]
async fn a_failed_build_is_reported_once_per_commit() {
    let fixture = Fixture::new().await;
    let (checkout, repo) = seeded(&fixture, "#!/bin/sh\necho broken\nexit 3\n").await;
    let layout = Layout::of(&repo);
    // A strained host defers the build.
    fixture.daemon.strain(true);
    refresh(&fixture).await;
    assert!(!layout.log().exists(), "the build waits for headroom");
    fixture.daemon.strain(false);
    refresh(&fixture).await;
    let log = fs::read_to_string(layout.log()).unwrap();
    assert!(log.contains("broken\n"), "{log}");
    fs::remove_file(layout.log()).unwrap();
    refresh(&fixture).await;
    assert!(!layout.log().exists(), "the failed commit is not rebuilt");
    // Other settings at the same commit are a new build.
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n[env]\nMODE = 'seed'\n",
    )
    .unwrap();
    refresh(&fixture).await;
    assert!(layout.log().exists(), "changed settings rebuild");
    // Returning to the failed settings retries them.
    fs::remove_file(layout.log()).unwrap();
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n",
    )
    .unwrap();
    refresh(&fixture).await;
    assert!(layout.log().exists(), "a revisited failure is retried");
    assert!(!seed_failures(&fixture).await.is_empty());
    assert!(Snapshot::open(&layout).unwrap().is_none());
}

#[tokio::test]
async fn a_preset_builds_and_keeps_its_tool_outputs() {
    let fixture = Fixture::new().await;
    let checkout = repository(fixture.root.path(), "repo");
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
    let repo = fixture.register(checkout.display()).await;
    refresh(&fixture).await;
    let layout = Layout::of(&repo);
    let log = fs::read_to_string(layout.log()).unwrap();
    let built = layout.worktree().join("target/debug/libseeded.rlib");
    assert!(built.is_file(), "{log}");
    // The first build resets what an earlier one could have left.
    assert!(log.starts_with("$ rustc -vV\n"), "{log}");
    assert!(log.contains("$ cargo clean --workspace\n"), "{log}");
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&fixture, &layout).await;
    }
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert!(snapshot.toolchains[&Preset::Cargo].starts_with("rustc "));
    let workspace = start(&fixture, &repo, "feature").await;
    assert!(workspace.path.join("target/debug").is_dir());

    // A change to the code alone builds on the outputs.
    fs::write(checkout.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    commit(&checkout, "src/lib.rs");
    refresh(&fixture).await;
    let log = fs::read_to_string(layout.log()).unwrap();
    assert!(!log.contains("cargo clean"), "{log}");
    assert!(log.contains("$ cargo clippy --all-targets\n"), "{log}");
}

/// A repository whose seed runs `presets` with the fake tools of its `bin`:
/// `cargo` records each command in `target` and fails at `clippy` as a
/// toolchain without clippy does, and `npm ci` exits with `npm_exit`.
async fn fake_tools(fixture: &Fixture, presets: &str, npm_exit: i32) -> (PathBuf, Repository) {
    let checkout = repository(fixture.root.path(), "repo");
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
    let repo = fixture.register(checkout.display()).await;
    (checkout, repo)
}

#[tokio::test]
async fn a_failed_soft_step_keeps_the_other_steps_outputs() {
    let fixture = Fixture::new().await;
    let (checkout, repo) = fake_tools(&fixture, "['cargo']", 0).await;
    refresh(&fixture).await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&fixture, &layout).await;
    }
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert_eq!(
        snapshot.commit,
        git(&checkout, &["rev-parse", "main"]).trim()
    );
    let failures = seed_failures(&fixture).await;
    assert!(
        (failures.iter())
            .any(|message| message.contains("cargo clippy --all-targets exited with 101")),
        "{failures:?}"
    );
    let workspace = start(&fixture, &repo, "feature").await;
    for built in ["build", "test", "check"] {
        assert!(
            workspace.path.join("target").join(built).is_file(),
            "{built}"
        );
    }
    // The snapshot is current, so the same tip and settings do not rebuild.
    fs::remove_file(layout.log()).unwrap();
    refresh(&fixture).await;
    assert!(!layout.log().exists(), "the seed is not rebuilt");
}

#[tokio::test]
async fn a_failure_after_a_soft_one_fails_the_build() {
    let fixture = Fixture::new().await;
    let (_, repo) = fake_tools(&fixture, "['cargo', 'npm']", 1).await;
    refresh(&fixture).await;
    let layout = Layout::of(&repo);
    let log = fs::read_to_string(layout.log()).unwrap();
    assert!(log.contains("$ npm ci\n"), "{log}");
    assert!(Snapshot::open(&layout).unwrap().is_none());
    // The build's failure is the report; the soft one shows only in the log.
    let failures = seed_failures(&fixture).await;
    assert!(
        (failures.iter()).any(|message| message.contains("npm ci exited with 1")),
        "{failures:?}"
    );
    assert!(
        !(failures.iter()).any(|message| message.contains("clippy")),
        "{failures:?}"
    );
}

#[tokio::test]
async fn new_workspaces_take_the_spare_which_is_then_refilled() {
    let fixture = Fixture::new().await;
    let script = "#!/bin/sh\nmkdir -p out\necho built > out/built\n";
    let (_, repo) = seeded(&fixture, script).await;
    refresh(&fixture).await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&fixture, &layout).await;
    }
    let repositories = &fixture.repositories;
    // The new snapshot asked for its spare.
    let wanted = repositories.seeds().spare_wanted();
    (tokio::time::timeout(Duration::from_secs(60), wanted).await).expect("the build wants a spare");
    repositories.refill_spares(&fixture.daemon).await;
    assert!(layout.has_spare());
    let workspace = start(&fixture, &repo, "feature").await;
    assert_eq!(
        fs::read_to_string(workspace.path.join("out/built")).unwrap(),
        "built\n"
    );
    assert!(!layout.has_spare());
    let wanted = repositories.seeds().spare_wanted();
    (tokio::time::timeout(Duration::from_secs(60), wanted).await)
        .expect("taking the spare wants another");
    repositories.refill_spares(&fixture.daemon).await;
    assert!(layout.has_spare());
}

#[tokio::test]
async fn repository_removal_cancels_a_running_build() {
    let fixture = Fixture::new().await;
    let script = "#!/bin/sh\ntouch started\nexec sleep 600\n";
    let (_, repo) = seeded(&fixture, script).await;
    let layout = Layout::of(&repo);
    let refresh = refresh(&fixture);
    let removal = async {
        // The command starts only while its build holds the slot.
        let started = layout.worktree().join("started");
        let wait = async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        (tokio::time::timeout(Duration::from_secs(60), wait).await).expect("the build starts");
        let repositories = &fixture.repositories;
        repositories
            .remove(&fixture.daemon, &repo.id)
            .await
            .unwrap();
        assert!(!layout.root().exists());
    };
    let refreshed = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(refresh, removal)
    });
    refreshed.await.expect("the cancelled build returns");
}

#[tokio::test]
async fn a_removed_seed_is_neither_applied_nor_kept() {
    let fixture = Fixture::new().await;
    let script = "#!/bin/sh\nmkdir -p out\ntouch out/built\n";
    let (checkout, repo) = seeded(&fixture, script).await;
    refresh(&fixture).await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&fixture, &layout).await;
    }
    assert!(Snapshot::open(&layout).unwrap().is_some());
    // Outputs built under other settings are not applied.
    fs::write(
        checkout.join(".shoal.toml"),
        "[setup.seed]\ncommand = 'seed.sh'\npaths = ['out']\n[env]\nMODE = 'other'\n",
    )
    .unwrap();
    commit(&checkout, ".shoal.toml");
    let changed = start(&fixture, &repo, "changed").await;
    assert!(!changed.path.join("out").exists());
    git(&checkout, &["rm", "-q", ".shoal.toml"]);
    git(&checkout, &["commit", "-q", "-m", "Drop the seed"]);
    let workspace = start(&fixture, &repo, "feature").await;
    assert!(!workspace.path.join("out").exists());
    refresh(&fixture).await;
    assert!(!layout.root().exists());
}

#[tokio::test]
async fn removal_keeps_a_seed_worktree_shoal_did_not_create() {
    let fixture = Fixture::new().await;
    let (_, repo) = seeded(&fixture, "#!/bin/sh\n").await;
    let layout = Layout::of(&repo);
    fs::create_dir_all(layout.worktree()).unwrap();
    fs::write(layout.worktree().join("keep"), "user data").unwrap();
    let repositories = &fixture.repositories;
    assert!(
        repositories
            .remove(&fixture.daemon, &repo.id)
            .await
            .is_err()
    );
    assert!(layout.worktree().join("keep").exists());
}

#[tokio::test]
async fn processes_a_build_leaves_behind_are_stopped() {
    let fixture = Fixture::new().await;
    let script = "#!/bin/sh\nsleep 600 &\necho $! > ../leftover\nmkdir -p out\n";
    let (_, repo) = seeded(&fixture, script).await;
    refresh(&fixture).await;
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
    let fixture = Fixture::new().await;
    let (_, repo) = seeded(&fixture, "#!/bin/sh\nmkdir -p out\n").await;
    refresh(&fixture).await;
    let layout = Layout::of(&repo);
    if !layout.worktree().exists() {
        return; // The seed was not created on this filesystem.
    }
    // The real worktree moves away and a symlink takes its place.
    let moved = fixture.root.path().join("moved");
    fs::rename(layout.worktree(), &moved).unwrap();
    git(&repo.path, &["worktree", "repair", moved.to_str().unwrap()]);
    std::os::unix::fs::symlink(&moved, layout.worktree()).unwrap();
    let repositories = &fixture.repositories;
    assert!(
        repositories
            .remove(&fixture.daemon, &repo.id)
            .await
            .is_err()
    );
    assert!(moved.join("seed.sh").exists());
}

#[tokio::test]
async fn a_start_clones_while_a_refill_reads_the_snapshot() {
    let fixture = Fixture::new().await;
    let script = "#!/bin/sh\nmkdir -p out\necho built > out/built\n";
    let (_, repo) = seeded(&fixture, script).await;
    refresh(&fixture).await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&fixture, &layout).await;
    }
    // A refill reads the snapshot for its whole clone.
    let slot = fixture.repositories.seeds().slot(&repo.id);
    let _refill = slot.snapshot().clone().read_owned().await;
    let started = start(&fixture, &repo, "feature");
    let workspace = tokio::time::timeout(Duration::from_secs(60), started)
        .await
        .expect("the start does not wait for the refill");
    assert_eq!(
        fs::read_to_string(workspace.path.join("out/built")).unwrap(),
        "built\n"
    );
}
