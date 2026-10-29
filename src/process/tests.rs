use super::*;

#[tokio::test]
async fn in_directory_leaves_out_excluded_processes() {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new("sleep")
        .arg("60")
        .current_dir(root.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let named = format!("(PID {pid})");
    let found = in_directory(root.path()).await.unwrap();
    assert!(found.iter().any(|process| process.ends_with(&named)));
    let found = in_directory_except(root.path(), &[pid]).await.unwrap();
    assert!(!found.iter().any(|process| process.ends_with(&named)));
    child.kill().await.unwrap();
}

#[tokio::test]
async fn detached_in_directory_finds_adopted_processes_and_their_children_there() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("worktree");
    std::fs::create_dir(&dir).unwrap();
    let detached = crate::test_support::Detached::start(&dir, &root.path().join("pids")).await;
    let mut attached = Command::new("sleep")
        .arg("60")
        .current_dir(&dir)
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let found: Vec<u32> = detached_in_directory(&dir)
        .await
        .unwrap()
        .into_iter()
        .map(|identity| identity.pid)
        .collect();

    assert!(found.contains(&detached.shell), "{found:?}");
    assert!(found.contains(&detached.child), "{found:?}");
    assert!(!found.contains(&attached.id().unwrap()), "{found:?}");
    attached.kill().await.unwrap();
}
