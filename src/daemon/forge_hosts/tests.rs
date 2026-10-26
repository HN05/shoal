use std::{sync::Arc, time::Duration};

use crate::{
    config::Config,
    forge::{ForgeRepo, backends::ForgeKind, hosts::HostType},
};

#[tokio::test]
async fn a_lookup_waiting_on_the_host_takes_the_answer_the_first_one_records() {
    let (_root, manager) = crate::test_support::manager().await;
    let host = "waiting.invalid";
    let asking = manager.lock_forge_host(host).await;
    let gate = manager.forge_host_gates.lock().await[host].clone();
    let waiting = tokio::spawn({
        let manager = manager.clone();
        async move { manager.host_type(host, "https").await }
    });
    // The map, the asking lookup, this test and the waiting lookup.
    tokio::time::timeout(Duration::from_secs(60), async {
        while Arc::strong_count(&gate) < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second lookup reaches the host's gate");
    manager
        .record_forge_host(host, ForgeKind::GitHub, "https")
        .await
        .unwrap();
    drop(asking);
    // Asking the host itself would fail: it does not resolve.
    assert_eq!(
        waiting.await.unwrap().unwrap(),
        HostType::Answered(ForgeKind::GitHub)
    );
}

#[tokio::test]
async fn recorded_hosts_parse_as_their_forge_once_the_daemon_learns_them() {
    let (_root, manager) = crate::test_support::manager().await;
    let url = "https://ghe.recorded.example/team/repo/issues/7";
    manager
        .record_forge_host("ghe.recorded.example", ForgeKind::GitHub, "https")
        .await
        .unwrap();
    manager.learn_recorded_forge_hosts().await.unwrap();
    let repo = ForgeRepo::from_issue_url(url).unwrap();
    assert_eq!(
        repo.pull("8").unwrap().1,
        "https://ghe.recorded.example/team/repo/pull/8"
    );
}

#[tokio::test]
async fn reloaded_pins_reach_link_parsing_and_removed_ones_leave_it() {
    let (_root, manager) = crate::test_support::manager().await;
    let url = "https://gitlab.reload.example/team/repo/issues/7";
    let config = Config {
        forges: toml::from_str("[\"gitlab.reload.example\"]\ntype = \"forgejo\"\n").unwrap(),
        ..Config::default()
    };
    manager.publish_config(config);
    assert!(ForgeRepo::from_issue_url(url).unwrap().issue(url).is_ok());
    // An answer a probe resolved before the reload does not replace it.
    crate::forge::hosts::learn("gitlab.reload.example", ForgeKind::GitLab);
    assert!(ForgeRepo::from_issue_url(url).unwrap().issue(url).is_ok());
    manager.publish_config(Config::default());
    // Without the pin, the host parses as what it was learned to serve.
    assert!(ForgeRepo::from_issue_url(url).unwrap().issue(url).is_err());
}
