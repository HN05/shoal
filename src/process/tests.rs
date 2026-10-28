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
