use std::fs;

use crate::{client::notifications::NotificationKind, daemon::test_support::manager};

/// The messages of the unread notifications of moved packages.
async fn moved_notices(manager: &crate::daemon::workspace::Manager) -> Vec<String> {
    (manager.notifications(true, 50).await.unwrap().into_iter())
        .filter(|notification| notification.kind == NotificationKind::UnmovedPackages)
        .map(|notification| notification.message)
        .collect()
}

#[tokio::test]
async fn kept_packages_move_to_the_root_dir_once_with_one_notification() {
    let (_root, manager) = manager().await;
    let kept = manager.paths.state.join("unmoved");
    fs::create_dir_all(&kept).unwrap();
    for name in ["chat-1.shoal", "workspace-2.shoal"] {
        fs::write(kept.join(name), name).unwrap();
    }

    manager.move_unmoved_packages().await.unwrap();
    manager.move_unmoved_packages().await.unwrap();

    assert!(!kept.exists());
    let target = (manager.config().root_dir(&manager.paths).unwrap()).join(".unmoved");
    for name in ["chat-1.shoal", "workspace-2.shoal"] {
        assert_eq!(fs::read_to_string(target.join(name)).unwrap(), name);
    }
    let notices = moved_notices(&manager).await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    for name in ["chat-1.shoal", "workspace-2.shoal"] {
        let file = target.join(name);
        let command = crate::shell::quote(&["shoal", "move", "import", file.to_str().unwrap()]);
        assert!(notices[0].contains(&command), "{notices:?}");
    }
}

#[tokio::test]
async fn a_package_whose_name_is_taken_there_stays() {
    let (_root, manager) = manager().await;
    let kept = manager.paths.state.join("unmoved");
    fs::create_dir_all(&kept).unwrap();
    fs::write(kept.join("chat-1.shoal"), "kept").unwrap();
    let target = (manager.config().root_dir(&manager.paths).unwrap()).join(".unmoved");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("chat-1.shoal"), "other").unwrap();

    manager.move_unmoved_packages().await.unwrap();

    assert_eq!(
        fs::read_to_string(kept.join("chat-1.shoal")).unwrap(),
        "kept"
    );
    assert_eq!(
        fs::read_to_string(target.join("chat-1.shoal")).unwrap(),
        "other"
    );
    assert!(moved_notices(&manager).await.is_empty());
}
