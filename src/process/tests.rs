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
async fn one_listing_scans_once_for_every_directory() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let users = DirectoryUsers::default();
    assert!(
        users
            .in_directory_except(first.path(), &[])
            .await
            .unwrap()
            .is_empty()
    );
    let mut child = Command::new("sleep")
        .arg("60")
        .current_dir(second.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let named = format!("(PID {})", child.id().unwrap());
    let listed = users.in_directory_except(second.path(), &[]).await.unwrap();
    assert!(
        !listed.iter().any(|process| process.ends_with(&named)),
        "a process started after the listing is not in it"
    );
    let rescanned = in_directory(second.path()).await.unwrap();
    assert!(rescanned.iter().any(|process| process.ends_with(&named)));
    child.kill().await.unwrap();
}
