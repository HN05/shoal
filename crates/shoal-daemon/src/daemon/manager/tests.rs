use super::*;

#[tokio::test]
async fn reload_replaces_the_global_config_only_when_valid() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let before = manager.config();
    crate::test_support::write_global_config(
        &manager.paths,
        "[machine.simulators.profiles.phone]\nruntime = 'iOS 27'\n",
    );
    assert!(manager.reload_config().await.is_err());
    assert!(Arc::ptr_eq(&manager.config(), &before));
    crate::test_support::write_global_config(
        &manager.paths,
        "[machine.simulators]\ndefault = 'phone'\n\
             [machine.simulators.profiles.phone]\ndevice = 'iPhone 17'\n",
    );
    manager.reload_config().await.unwrap();
    assert_eq!(
        manager.config().machine.simulators.default.as_deref(),
        Some("phone")
    );
    // A snapshot taken before the reload keeps the settings it started with.
    assert_eq!(before.machine.simulators.default, None);
}
