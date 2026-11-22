use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::daemon::{
    manager::Manager,
    test_support::{manager, repository},
};
use shoal_protocol::space_names::SpaceSelector;

const DAY: Duration = Duration::from_secs(24 * 3600);

/// A copy fetched `age` ago of a file of a workspace of a new repository.
async fn fetched(temp: &Path, manager: &Manager, name: &str, age: Duration) -> PathBuf {
    let checkout = repository(temp, name);
    let repo = (manager.repositories)
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "charts".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let directory = manager.fetched_directory(None, &selector).await.unwrap();
    let file = directory.path.join("7");
    std::fs::create_dir(&file).unwrap();
    let path = file.join("chart.html");
    std::fs::write(&path, "<html>").unwrap();
    let at = SystemTime::now() - age;
    for written in [&path, &file] {
        std::fs::File::open(written)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }
    path
}

#[tokio::test]
async fn copies_go_once_fetched_longer_ago_than_the_retention() {
    let (temp, manager) = manager().await;
    // `cleanup.restore.retention_days` is 30 by default.
    let expired = fetched(temp.path(), &manager, "old", 31 * DAY).await;
    let kept = fetched(temp.path(), &manager, "new", 29 * DAY).await;

    manager.expire_fetched().await.unwrap();

    assert!(!expired.exists());
    assert!(kept.exists());
}
