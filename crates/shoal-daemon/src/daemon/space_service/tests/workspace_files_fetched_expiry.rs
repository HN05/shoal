use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::daemon::{
    manager::Manager,
    test_support::{manager, repository},
};
use shoal_protocol::space_names::SpaceSelector;

const DAY: Duration = Duration::from_secs(24 * 3600);

/// A copy fetched `age` ago, and stamped then, of a file of a workspace of a
/// new repository.
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
    let directory = manager
        .spaces
        .fetched_directory(manager, None, &selector)
        .await
        .unwrap();
    let file = directory.path.join("7");
    std::fs::create_dir(&file).unwrap();
    let path = file.join("chart.html");
    std::fs::write(&path, "<html>").unwrap();
    let since = (SystemTime::now() - age)
        .duration_since(UNIX_EPOCH)
        .unwrap();
    let at = UNIX_EPOCH + Duration::from_secs(since.as_secs());
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

    manager.spaces.expire_fetched(&*manager).await.unwrap();

    assert!(!expired.exists());
    assert!(kept.exists());
}

#[tokio::test]
async fn disk_space_cleanup_deletes_unchanged_copies_where_it_frees_space() {
    let (temp, manager) = manager().await;
    let low = fetched(temp.path(), &manager, "low", DAY).await;
    let elsewhere = fetched(temp.path(), &manager, "elsewhere", DAY).await;
    let low_dir = (low.ancestors().nth(3)).unwrap().to_owned();

    assert!(
        manager
            .spaces
            .delete_fetched_in(&*manager, |path| path == low_dir)
            .await
    );

    assert!(!low.exists());
    assert!(elsewhere.exists());
}
