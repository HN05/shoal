use shoal_core::config::Config;
use shoal_protocol::hub::sharing;

use super::*;
use crate::{
    sharing::{To, copies},
    stand_in::{self, Registered, StandIn},
};

const APP: &str = "https://example.test/team/app.git";
const OTHER: &str = "https://example.test/team/other.git";
const VALID: &str = "[setup]\ncommand = \"make\"\n";

/// The hub service over a temporary store and home, on the hub's own
/// machine, with the stand-in daemon and `registered` repositories.
async fn hub(
    root: &tempfile::TempDir,
    registered: Vec<Registered>,
) -> (Arc<crate::Hub>, Arc<StandIn>) {
    let config = Config::from_toml("[machine.hub]\nenabled = true\n").unwrap();
    let hub = stand_in::hub(root, config).await;
    (hub, Arc::new(StandIn::registering(registered)))
}

/// The repository `id` cloned from `source`, shared under `shared_url`,
/// with the local config `local_config`.
fn registered(
    id: &str,
    source: &str,
    shared_url: Option<&str>,
    local_config: Option<&str>,
) -> Registered {
    let repository = Repository {
        id: id.into(),
        path: format!("/repositories/{id}/.checkout").into(),
        source: source.into(),
        last_used: 0,
        name: Some(id.into()),
        workspaces_dir: format!("/repositories/{id}").into(),
        contribution: false,
        shared_url: shared_url.map(Into::into),
    };
    let local_config = local_config.map(Into::into);
    Registered {
        repository,
        local_config,
    }
}

fn copy(url: &str, text: Option<&str>, revision: i64) -> Copy<SharedLocalConfigs> {
    sharing::Copy {
        key: Origin::of(url),
        value: text.map(Into::into),
        revision,
    }
}

/// The local config of each registration of `daemon`, by its ID.
fn local_configs_of(daemon: &StandIn) -> Vec<(String, Option<String>)> {
    (daemon.registered().into_iter())
        .map(|r| (r.repository.id.to_string(), r.local_config))
        .collect()
}

async fn alerts_of(hub: &crate::Hub, kind: AlertKind) -> Vec<String> {
    let alerts = hub.notifications.list(false, 100).await.unwrap();
    (alerts.into_iter())
        .filter(|alert| alert.kind == kind)
        .map(|alert| alert.message)
        .collect()
}

/// The hub's copy of every shared local config.
async fn hub_copies(hub: &crate::Hub) -> Vec<Copy<SharedLocalConfigs>> {
    hub.store
        .read(copies::list::<SharedLocalConfigs>)
        .await
        .unwrap()
}

#[tokio::test]
async fn only_a_shared_repository_s_local_config_goes_to_the_hub() {
    let root = tempfile::tempdir().unwrap();
    let registrations = vec![
        registered("app", APP, Some(APP), Some(VALID)),
        registered("other", OTHER, None, Some("[setup]\n")),
    ];
    let (hub, daemon) = hub(&root, registrations).await;
    let daemon: Arc<dyn Daemon> = daemon;
    let own = To::Own(hub.own_member().await.unwrap());
    for knows in [false, true] {
        let round = hub.share_round::<SharedLocalConfigs>(&daemon, &own, knows);
        round.await.unwrap();
    }
    assert_eq!(hub_copies(&hub).await, [copy(APP, Some(VALID), 1)]);
}

#[tokio::test]
async fn a_file_no_longer_shared_is_no_edit() {
    let root = tempfile::tempdir().unwrap();
    // Agreed on while shared, the file has changed since it was unshared.
    let registrations = vec![registered("app", APP, None, Some("[setup]\n"))];
    let (hub, daemon) = hub(&root, registrations).await;
    let agreed = Agreed::<SharedLocalConfigs>::from([(Origin::of(APP), copy(APP, Some(VALID), 4))]);
    let items = local_configs(&*daemon, &agreed, &BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(
        items,
        Items::<SharedLocalConfigs>::from([(Origin::of(APP), VALID.to_owned())])
    );
    // Removed, it is no edit either.
    let removed = Agreed::<SharedLocalConfigs>::from([(Origin::of(APP), copy(APP, None, 5))]);
    assert_eq!(
        local_configs(&*daemon, &removed, &BTreeSet::new())
            .await
            .unwrap(),
        Items::<SharedLocalConfigs>::new()
    );
    drop(hub);
}

#[tokio::test]
async fn a_copy_replaces_shared_registrations_files_and_is_agreed_on_for_other_origins() {
    let root = tempfile::tempdir().unwrap();
    let registrations = vec![
        registered("app", APP, Some(APP), Some("[setup]\n")),
        registered("other", OTHER, None, Some("[setup]\n")),
    ];
    let (hub, daemon) = hub(&root, registrations).await;
    let stand_in = daemon.clone();
    let daemon: Arc<dyn Daemon> = daemon;
    let copies = vec![copy(APP, Some(VALID), 2), copy(OTHER, Some(VALID), 3)];
    let take = hub.take_sent_copies::<SharedLocalConfigs>(&daemon, true, copies.clone());
    take.await.unwrap();
    assert_eq!(
        local_configs_of(&stand_in),
        [
            ("app".into(), Some(VALID.into())),
            ("other".into(), Some("[setup]\n".into()))
        ]
    );
    let [kept] = alerts_of(&hub, AlertKind::SharedFileKept)
        .await
        .try_into()
        .unwrap();
    assert!(
        kept.contains("/repositories/app/config.toml.backup"),
        "{kept}"
    );
    let key = hub.sharing_hub_key(&*daemon).await.unwrap().unwrap();
    let agreed = hub.agreed_copies::<SharedLocalConfigs>(&key).await.unwrap();
    assert_eq!(agreed.into_values().collect::<Vec<_>>(), copies);
}

#[tokio::test]
async fn a_registration_that_comes_to_be_shared_takes_the_hub_s_copy() {
    let root = tempfile::tempdir().unwrap();
    let registrations = vec![
        registered("app", APP, None, Some("[setup]\n")),
        registered("fresh", OTHER, None, None),
    ];
    let (hub, daemon) = hub(&root, registrations).await;
    let stand_in = daemon.clone();
    let daemon: Arc<dyn Daemon> = daemon;
    let copies = vec![copy(APP, Some(VALID), 2), copy(OTHER, Some(VALID), 3)];
    let take = hub.take_sent_copies::<SharedLocalConfigs>(&daemon, true, copies);
    take.await.unwrap();
    for (id, url) in [("app", APP), ("fresh", OTHER)] {
        let id = RepositoryId::from(id);
        daemon
            .share_repository_as(&id, Some(url.into()))
            .await
            .unwrap();
        (hub.shared_registration_joined(&*daemon, &id, &Origin::of(url))).await;
    }
    assert_eq!(
        local_configs_of(&stand_in),
        [
            ("app".into(), Some(VALID.into())),
            ("fresh".into(), Some(VALID.into()))
        ]
    );
    // Only the file of its own that it replaced is kept.
    let [kept] = alerts_of(&hub, AlertKind::SharedFileKept)
        .await
        .try_into()
        .unwrap();
    assert!(
        kept.contains("/repositories/app/config.toml.backup"),
        "{kept}"
    );
}

#[tokio::test]
async fn a_registration_shared_without_a_copy_goes_only_once_the_hub_s_copies_are_read() {
    let root = tempfile::tempdir().unwrap();
    let registrations = vec![registered("app", APP, None, Some(VALID))];
    let (hub, daemon) = hub(&root, registrations).await;
    let daemon: Arc<dyn Daemon> = daemon;
    let own = To::Own(hub.own_member().await.unwrap());
    let round = || hub.share_round::<SharedLocalConfigs>(&daemon, &own, true);
    round().await.unwrap();
    let id = RepositoryId::from("app");
    daemon
        .share_repository_as(&id, Some(APP.into()))
        .await
        .unwrap();
    (hub.shared_registration_joined(&*daemon, &id, &Origin::of(APP))).await;
    // The round that reads the hub's copies, which may hold one on its way
    // here, sends nothing; the next sends the file.
    round().await.unwrap();
    assert_eq!(hub_copies(&hub).await, []);
    round().await.unwrap();
    assert_eq!(hub_copies(&hub).await, [copy(APP, Some(VALID), 1)]);
}

#[tokio::test]
async fn an_invalid_local_config_stays_on_its_machine_without_holding_back_the_others() {
    let root = tempfile::tempdir().unwrap();
    let registrations = vec![
        registered("app", APP, Some(APP), Some("cleanup = 'often'\n")),
        registered("other", OTHER, Some(OTHER), Some(VALID)),
    ];
    let (hub, daemon) = hub(&root, registrations).await;
    let daemon: Arc<dyn Daemon> = daemon;
    let own = To::Own(hub.own_member().await.unwrap());
    for knows in [false, true, true] {
        let round = hub.share_round::<SharedLocalConfigs>(&daemon, &own, knows);
        round.await.unwrap();
    }
    assert_eq!(hub_copies(&hub).await, [copy(OTHER, Some(VALID), 1)]);
    let [problem] = alerts_of(&hub, AlertKind::EnvironmentProblem)
        .await
        .try_into()
        .unwrap();
    assert!(problem.contains(Origin::of(APP).as_str()), "{problem}");
}
