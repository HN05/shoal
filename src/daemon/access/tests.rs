use super::*;

#[tokio::test]
async fn a_writable_directory_or_its_existing_ancestor_passes_and_keeps_no_file() {
    let root = tempfile::tempdir().unwrap();
    ensure_writable(root.path()).await.unwrap();
    ensure_writable(&root.path().join("missing/below"))
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn a_failed_file_names_the_directory() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, "").unwrap();
    let directory = file.join("exports");
    let error = ensure_writable(&directory).await.unwrap_err();
    assert!(
        format!("{error:#}").starts_with(&format!(
            "the Shoal daemon cannot write to {}: ",
            directory.display()
        )),
        "{error:#}"
    );
}

#[tokio::test]
async fn a_file_that_cannot_be_created_in_time_fails_at_the_limit() {
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let error = within_limit(Path::new("/volume"), Duration::from_secs(2), move |_| {
        let _ = blocked.recv();
        Ok(())
    })
    .await
    .unwrap_err();
    drop(release);
    assert_eq!(
        format!("{error:#}"),
        "the Shoal daemon cannot write to /volume: creating a file there did not finish within 2 seconds; macOS may not have given the daemon access to that volume"
    );
}
