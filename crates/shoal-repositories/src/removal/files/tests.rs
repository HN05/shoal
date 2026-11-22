use super::delete_local_config;

#[tokio::test]
async fn the_local_config_goes_only_when_it_is_the_repositorys_own_directory() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("config.toml"), "").unwrap();
    let linked = root.path().join("linked");
    std::fs::create_dir(&linked).unwrap();
    std::os::unix::fs::symlink(&elsewhere, linked.join(".shoal")).unwrap();
    delete_local_config(linked.clone()).await.unwrap();
    assert!(elsewhere.join("config.toml").exists());
    // A redirected ancestor leads elsewhere too.
    let target = root.path().join("target");
    std::fs::create_dir_all(target.join("repo/.shoal")).unwrap();
    std::fs::write(target.join("repo/.shoal/config.toml"), "").unwrap();
    std::os::unix::fs::symlink(&target, root.path().join("redirect")).unwrap();
    delete_local_config(root.path().join("redirect/repo"))
        .await
        .unwrap();
    assert!(target.join("repo/.shoal/config.toml").exists());
    // A Git checkout there holds tracked files.
    let canonical = std::fs::canonicalize(root.path()).unwrap();
    let checkout = canonical.join("checkout");
    std::fs::create_dir_all(checkout.join(".shoal")).unwrap();
    std::fs::write(checkout.join(".shoal/.git"), "gitdir: elsewhere\n").unwrap();
    delete_local_config(checkout.clone()).await.unwrap();
    assert!(checkout.join(".shoal/.git").exists());

    let owned = canonical.join("owned");
    std::fs::create_dir_all(owned.join(".shoal")).unwrap();
    std::fs::write(owned.join(".shoal/config.toml"), "").unwrap();
    delete_local_config(owned.clone()).await.unwrap();
    assert!(!owned.join(".shoal").exists());
}
