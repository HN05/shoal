//! A reload's change to overload protection, which the daemon's config
//! publication makes the agents' recovery start over for.

/// Publish a calm config, then `tighten` it once recovery is ready: the
/// reload withdraws readiness before any agent can use it. The monitor's
/// tests cover the new window it then waits for.
async fn a_reload_withdraws_readiness(tighten: fn(&mut crate::config::Config)) {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let calm = || {
        let mut config = crate::config::Config::default();
        config.machine.overload.memory.enabled = false;
        config.machine.overload.disk.enabled = false;
        config
    };
    let mut tightened = calm();
    tighten(&mut tightened);
    manager.publish_config(calm());
    let recovery = manager.agents.overload_recovery();
    recovery.publish(Some(recovery.epoch()));
    manager.publish_config(tightened);
    assert_eq!(recovery.published(), None);
    assert_eq!(*recovery.watch().borrow(), None);
}

#[tokio::test]
async fn a_reload_of_a_threshold_withdraws_published_readiness() {
    a_reload_withdraws_readiness(|config| {
        config.machine.overload.recovery.memory_used_percent = 75;
    })
    .await;
}

#[tokio::test]
async fn a_reload_of_the_memory_limit_withdraws_published_readiness() {
    a_reload_withdraws_readiness(|config| {
        config.machine.memory_limit = crate::config::memory_limit::MemoryLimitSetting::Limit(
            crate::config::memory_limit::MemoryLimit::Bytes(8 << 30),
        );
    })
    .await;
}
