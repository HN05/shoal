use std::{fs, path::Path, time::Duration};

use shoal_repositories::seed::{Layout, Snapshot};

use crate::daemon::test_support::{
    commit, git, install_test_script, manager, native_cloning, repository,
};
use crate::daemon::{manager::Manager, notifications::AlertKind};
use crate::model::Repository;
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

/// Admit what the agent queue admits, seed builds among it, as the daemon's
/// dispatcher does, until the returned task is dropped.
fn admitting(manager: &std::sync::Arc<Manager>) -> AbortOnDrop {
    let dispatching = crate::daemon::queued_starts::run(manager.agents.clone(), manager.clone());
    AbortOnDrop(tokio::spawn(dispatching))
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
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
    let _admitting = admitting(&manager);
    manager.repositories.refresh_seeds(&*manager).await;
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
    manager.repositories.refresh_seeds(&*manager).await;
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
    let untracked = manager.spaces.untracked_worktrees(&*manager).await.unwrap();
    assert!(untracked.is_empty(), "{untracked:?}");

    manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();
    assert!(!layout.root().exists());
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
            if let Some(workspace) = manager.spaces.list_workspaces().await.unwrap().pop() {
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
    let workspace = manager.spaces.workspace_by_id(&id).await.unwrap();
    assert_eq!(workspace.state, WorkspaceState::Preparing);
    crate::test_support::assert_pending(manager.gates.wait_for_creation(&repo.id, &id)).await;
    drop(swap);
    let workspace = creating.await.unwrap().unwrap();
    assert_eq!(workspace.state, WorkspaceState::Ready);
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
        .spaces
        .take_checkpoint(&*manager, &SpaceSelector::id(&source.id), None)
        .await
        .unwrap();
    let _admitting = admitting(&manager);
    manager.repositories.refresh_seeds(&*manager).await;
    let layout = Layout::of(&repo);
    if !native_cloning(layout.root()) {
        return assert_clone_refused(&manager, &layout).await;
    }
    let request = crate::protocol::CopySource {
        workspace: SpaceSelector::id(&source.id),
        checkpoint: Some(checkpoint.number),
    };
    let copy = manager
        .spaces
        .create_copy(
            &*manager,
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
