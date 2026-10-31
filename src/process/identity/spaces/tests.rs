use super::*;
use crate::test_support::{Adopted, Detached, isolated_command, runs_until_stopped};

/// This test binary, which runs until stopped as [`runs_until_stopped`]
/// tells it to.
fn this_binary() -> PathBuf {
    std::env::current_exe().unwrap()
}

/// A workspace's directories under `root`, which hold no process yet.
fn workspace(root: &Path) -> Space {
    let worktree = root.join("worktree");
    let tmp_dir = root.join("tmp");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&tmp_dir).unwrap();
    Space {
        id: "workspace-id".into(),
        worktree: Some(worktree),
        tmp_dir: Some(tmp_dir),
    }
}

async fn untracked(space: &Space, tracked: Vec<Identity>) -> Vec<Untracked> {
    let mut found = survey(vec![space.clone()], tracked).await.unwrap();
    found.remove(&space.id).unwrap()
}

/// Whether `found` lists `identity`, and as a leftover or not.
fn listed(found: &[Untracked], identity: &Identity) -> Option<bool> {
    found
        .iter()
        .find(|process| process.identity == *identity)
        .map(|process| process.leftover)
}

#[tokio::test]
async fn detached_processes_in_the_worktree_are_leftovers_and_attached_ones_are_not_found() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let worktree = space.worktree.clone().unwrap();
    let detached = Detached::start(&worktree, &root.path().join("pids")).await;
    let attached = isolated_command("sleep", &worktree)
        .arg("300")
        .spawn()
        .unwrap();
    let attached = super::super::capture(attached.id().unwrap())
        .unwrap()
        .unwrap();

    let found = untracked(&space, vec![]).await;

    assert_eq!(listed(&found, &detached.shell), Some(true), "{found:?}");
    assert_eq!(listed(&found, &detached.child), Some(true), "{found:?}");
    // A process a live parent keeps, such as a shell the user opened there,
    // is not the space's.
    assert_eq!(listed(&found, &attached), None, "{found:?}");
}

#[tokio::test]
async fn the_temporary_directory_and_programs_built_in_the_worktree_tie_detached_processes() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let tmp_dir = space.tmp_dir.clone().unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let built = space.worktree.as_ref().unwrap().join("built");
    std::fs::copy(this_binary(), &built).unwrap();

    let in_tmp = Adopted::start(&tmp_dir, "sleep 300", &[], &root.path().join("tmp-pid")).await;
    let running_built = Adopted::start(
        &elsewhere,
        &runs_until_stopped(&built),
        &[],
        &root.path().join("built-pid"),
    )
    .await;
    let unrelated = Adopted::start(&elsewhere, "sleep 300", &[], &root.path().join("pid")).await;

    let found = untracked(&space, vec![]).await;

    assert_eq!(listed(&found, &in_tmp.identity), Some(true), "{found:?}");
    assert_eq!(
        listed(&found, &running_built.identity),
        Some(true),
        "{found:?}"
    );
    assert_eq!(listed(&found, &unrelated.identity), None, "{found:?}");
}

#[tokio::test]
async fn the_identity_marker_finds_attached_processes_and_ties_detached_ones() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let marker = [(crate::env::WORKSPACE_ID, space.id.as_str())];
    let detached = Adopted::start(
        &elsewhere,
        &runs_until_stopped(&this_binary()),
        &marker,
        &root.path().join("pid"),
    )
    .await;
    let attached = isolated_command("sh", &elsewhere)
        .envs(marker)
        .args([
            "-c",
            &format!("exec {}", runs_until_stopped(&this_binary())),
        ])
        .spawn()
        .unwrap();
    let attached = super::super::capture(attached.id().unwrap())
        .unwrap()
        .unwrap();

    let found = untracked(&space, vec![]).await;

    assert_eq!(listed(&found, &detached.identity), Some(true), "{found:?}");
    assert_eq!(listed(&found, &attached), Some(false), "{found:?}");
}

#[tokio::test]
async fn a_leftover_includes_what_it_started_anywhere() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let worktree = space.worktree.clone().unwrap();
    let pids = root.path().join("pids");
    // The shell runs in the worktree; its child moved out of it.
    let script = format!(
        "sh -c '(cd / && exec sleep 300) & echo $$ $! > {pids}.tmp; mv {pids}.tmp {pids}; wait' \
         >/dev/null 2>&1 &",
        pids = pids.display()
    );
    let status = isolated_command("sh", &worktree)
        .args(["-c", &script])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let detached = Detached::recorded(&pids).await;

    let found = untracked(&space, vec![]).await;

    assert_eq!(listed(&found, &detached.shell), Some(true), "{found:?}");
    assert_eq!(listed(&found, &detached.child), Some(true), "{found:?}");
}

#[tokio::test]
async fn tracked_processes_their_descendants_and_their_ancestors_are_not_found() {
    let root = tempfile::tempdir().unwrap();
    let space = workspace(root.path());
    let worktree = space.worktree.clone().unwrap();
    let detached = Detached::start(&worktree, &root.path().join("pids")).await;

    let found = untracked(&space, vec![detached.shell.clone()]).await;
    assert_eq!(listed(&found, &detached.shell), None, "{found:?}");
    assert_eq!(listed(&found, &detached.child), None, "{found:?}");

    let found = untracked(&space, vec![detached.child.clone()]).await;
    assert_eq!(listed(&found, &detached.shell), None, "{found:?}");
    assert_eq!(listed(&found, &detached.child), None, "{found:?}");
}

#[tokio::test]
async fn a_chat_finds_processes_by_its_marker_and_temporary_directory() {
    let root = tempfile::tempdir().unwrap();
    let tmp_dir = root.path().join("tmp");
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&tmp_dir).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    let chat = Space {
        id: "chat-id".into(),
        worktree: None,
        tmp_dir: Some(tmp_dir.clone()),
    };
    let marker = [(crate::env::CHAT, "chat-id")];
    let marked = Adopted::start(
        &elsewhere,
        &runs_until_stopped(&this_binary()),
        &marker,
        &root.path().join("pid"),
    )
    .await;
    let in_tmp = Adopted::start(&tmp_dir, "sleep 300", &[], &root.path().join("tmp-pid")).await;

    let found = untracked(&chat, vec![]).await;

    assert_eq!(listed(&found, &marked.identity), Some(true), "{found:?}");
    assert_eq!(listed(&found, &in_tmp.identity), Some(true), "{found:?}");
}
