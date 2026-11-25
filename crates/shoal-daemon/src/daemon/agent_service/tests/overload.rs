//! A reload's change to overload protection, which the daemon's config
//! publication makes the agents' recovery start over for.
use crate::daemon::overload::*;
use std::time::Duration;
use tokio::time::Instant;

/// A hang guard on the paused clock, well past any recovery window.
async fn wait_published(ready: &mut tokio::sync::watch::Receiver<Option<u64>>) {
    tokio::time::timeout(Duration::from_secs(3600), ready.wait_for(Option::is_some))
        .await
        .expect("recovery was never published")
        .unwrap();
}

/// Publish a calm config, then `tighten` it once recovery is ready: the
/// reload withdraws readiness until a new window has passed.
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
    let mut ready = manager.agents.overload_recovery().watch();
    tokio::time::pause();
    let monitor = tokio::spawn(run(manager.agents.clone(), manager.clone()));
    wait_published(&mut ready).await;
    // Tightening withdraws readiness before any agent can use it.
    manager.publish_config(tightened);
    assert_eq!(*ready.borrow_and_update(), None);
    let reloaded = Instant::now();
    wait_published(&mut ready).await;
    let window = Duration::from_secs(manager.config().machine.overload.recovery.sustained_seconds);
    assert!(reloaded.elapsed() >= window);
    monitor.abort();
}

#[tokio::test]
async fn a_reload_of_a_threshold_withdraws_published_readiness_until_a_new_window() {
    a_reload_withdraws_readiness(|config| {
        config.machine.overload.recovery.memory_used_percent = 75;
    })
    .await;
}

#[tokio::test]
async fn a_reload_of_the_memory_limit_withdraws_published_readiness_until_a_new_window() {
    a_reload_withdraws_readiness(|config| {
        config.machine.memory_limit = crate::config::memory_limit::MemoryLimitSetting::Limit(
            crate::config::memory_limit::MemoryLimit::Bytes(8 << 30),
        );
    })
    .await;
}
