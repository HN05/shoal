use std::{sync::Arc, time::Duration};

use super::{Change, ForgeAccess, learn_forge_pins};
use crate::{
    config::Config,
    config_state::ConfigState,
    forge::{
        backends::ForgeKind,
        items::{link::LinkKind, reference::ItemRef},
    },
    gates::Gates,
};
use shoal_forge::hosts::HostType;

async fn access() -> (tempfile::TempDir, Arc<ForgeAccess>) {
    let temp = tempfile::tempdir().unwrap();
    let store = shoal_store::test_support::unsynced_store(temp.path().join("state.db"))
        .await
        .unwrap();
    let config = Arc::new(ConfigState::new(Config::default()));
    let access = ForgeAccess::new(store, Arc::default(), config);
    (temp, Arc::new(access))
}

#[tokio::test]
async fn a_lookup_waiting_on_the_host_takes_the_answer_the_first_one_records() {
    let (_temp, access) = access().await;
    let host = "waiting.invalid";
    let gates: &Gates = &access.gates;
    let asking = gates.lock_forge_host(host).await;
    let gate = gates.forge_host_gate(host);
    let waiting = tokio::spawn({
        let access = access.clone();
        async move { access.host_type(host, "https").await }
    });
    // The asking lookup, this test and the waiting lookup.
    tokio::time::timeout(Duration::from_secs(60), async {
        while Arc::strong_count(&gate) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second lookup reaches the host's gate");
    access
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
    let (_temp, access) = access().await;
    let url = "https://ghe.recorded.example/team/repo/issues/7";
    access
        .record_forge_host("ghe.recorded.example", ForgeKind::GitHub, "https")
        .await
        .unwrap();
    access.learn_recorded_forge_hosts().await.unwrap();
    let issue: ItemRef = url.parse().unwrap();
    let pull = ItemRef::numbered(issue.repository(), LinkKind::Pr, 8).unwrap();
    assert_eq!(pull.url(), "https://ghe.recorded.example/team/repo/pull/8");
}

#[test]
fn reloaded_pins_reach_link_parsing_and_removed_ones_leave_it() {
    let url = "https://gitlab.reload.example/team/repo/issues/7";
    let config =
        Config::from_toml("[machine.forges.\"gitlab.reload.example\"]\ntype = \"forgejo\"\n")
            .unwrap();
    let pins = &config.machine.forges;
    learn_forge_pins(&Default::default(), pins);
    assert!(url.parse::<ItemRef>().is_ok());
    // An answer a probe resolved before the reload does not replace it.
    crate::forge::hosts::learn("gitlab.reload.example", ForgeKind::GitLab);
    assert!(url.parse::<ItemRef>().is_ok());
    learn_forge_pins(pins, &Default::default());
    // Without the pin, the host parses as what it was learned to serve.
    assert!(url.parse::<ItemRef>().is_err());
}

#[test]
fn a_change_names_the_host_and_both_forges() {
    let change = Change {
        from: ForgeKind::GitHub,
        to: ForgeKind::GitLab,
    };
    let message = change.message("ghe.example");
    for named in ["ghe.example", "gitlab", "github"] {
        assert!(message.contains(named), "{message}");
    }
}
