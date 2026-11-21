use shoal_core::{config::Config, test_support::write_beside_global_config};

use super::*;
use crate::stand_in::{self, StandIn};

/// The hub service over a temporary store and home, with the stand-in
/// daemon.
async fn hub(root: &tempfile::TempDir) -> (Arc<crate::Hub>, Arc<dyn Daemon>) {
    let hub = stand_in::hub(root, Config::default()).await;
    (hub, Arc::new(StandIn))
}

fn member(name: &str) -> MemberId {
    MemberId::from(name.to_owned())
}

fn edit(file: FileName, text: Option<&str>) -> Edit {
    Edit {
        file,
        text: text.map(Into::into),
    }
}

fn copy(file: FileName, text: Option<&str>, revision: i64) -> Copy {
    Copy {
        file,
        text: text.map(Into::into),
        revision,
    }
}

/// The hub's copy of every file.
async fn hub_copies(hub: &crate::Hub) -> Vec<Copy> {
    hub.store.read(copies::list).await.unwrap()
}

async fn alerts_of(hub: &crate::Hub, kind: AlertKind) -> Vec<String> {
    let alerts = hub.notifications.list(false, 100).await.unwrap();
    (alerts.into_iter())
        .filter(|alert| alert.kind == kind)
        .map(|alert| alert.message)
        .collect()
}

fn read(hub: &crate::Hub, file: &str) -> Option<String> {
    let path = Config::path(&hub.paths).with_file_name(file);
    shoal_core::fsutil::read_optional(&path).unwrap()
}

#[tokio::test]
async fn the_hub_keeps_the_last_edit_of_each_file_that_changes_it() {
    let root = tempfile::tempdir().unwrap();
    let (hub, daemon) = hub(&root).await;
    let (desk, laptop) = (member("desk"), member("laptop"));
    let review = FileName::Mode("review".into());
    let edits = [
        (&desk, edit(FileName::Config, Some("a"))),
        (&desk, edit(review.clone(), Some("m"))),
        (&laptop, edit(FileName::Config, Some("b"))),
        // An edit to what the hub has already changes nothing.
        (&desk, edit(FileName::Config, Some("b"))),
        (&laptop, edit(review.clone(), None)),
    ];
    let mut copies = Vec::new();
    for (from, edit) in edits {
        copies = hub
            .take_shared_edits(&daemon, from, vec![edit])
            .await
            .unwrap();
    }
    assert_eq!(
        copies,
        [copy(review, None, 4), copy(FileName::Config, Some("b"), 3),]
    );
}

#[tokio::test]
async fn a_copy_that_replaces_an_unsent_edit_keeps_it_as_a_backup_once() {
    let root = tempfile::tempdir().unwrap();
    let (hub, daemon) = hub(&root).await;
    write_beside_global_config(&hub.paths, "shared.toml", "local\n");
    let take = |copies| hub.take_sent_copies(&*daemon, true, copies);
    take(vec![copy(FileName::Config, Some("hub\n"), 1)])
        .await
        .unwrap();
    assert_eq!(read(&hub, "shared.toml").as_deref(), Some("hub\n"));
    assert_eq!(read(&hub, "shared.toml.backup").as_deref(), Some("local\n"));
    let [kept] = alerts_of(&hub, AlertKind::SharedFileKept)
        .await
        .try_into()
        .unwrap();
    assert!(kept.contains("shared.toml.backup"), "{kept}");
    // A copy that replaces what the hub sent before keeps nothing, and an
    // older copy changes nothing.
    take(vec![copy(FileName::Config, Some("later\n"), 3)])
        .await
        .unwrap();
    take(vec![copy(FileName::Config, Some("older\n"), 2)])
        .await
        .unwrap();
    assert_eq!(read(&hub, "shared.toml").as_deref(), Some("later\n"));
    assert_eq!(read(&hub, "shared.toml.backup").as_deref(), Some("local\n"));
    assert_eq!(alerts_of(&hub, AlertKind::SharedFileKept).await.len(), 1);
    // What the hub wrote is no edit to send back.
    let own = To::Own(hub.own_member().await.unwrap());
    hub.share_round(&daemon, &own, true).await.unwrap();
    assert_eq!(hub_copies(&hub).await, []);
}

#[tokio::test]
async fn an_invalid_shared_config_stays_on_its_machine_until_it_is_fixed() {
    let root = tempfile::tempdir().unwrap();
    let (hub, daemon) = hub(&root).await;
    let own = To::Own(hub.own_member().await.unwrap());
    write_beside_global_config(&hub.paths, "shared.toml", "cleanup = 'often'\n");
    for _ in 0..2 {
        hub.share_round(&daemon, &own, true).await.unwrap();
    }
    assert_eq!(hub_copies(&hub).await, []);
    let problems = alerts_of(&hub, AlertKind::EnvironmentProblem).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    let valid = "[cleanup.auto]\nidle_minutes = 30\n";
    write_beside_global_config(&hub.paths, "shared.toml", valid);
    hub.share_round(&daemon, &own, true).await.unwrap();
    assert_eq!(
        hub_copies(&hub).await,
        [copy(FileName::Config, Some(valid), 1)]
    );
}

#[tokio::test]
async fn a_machine_reads_the_hub_s_copies_before_it_sends_a_file_the_hub_never_had() {
    let root = tempfile::tempdir().unwrap();
    let (hub, daemon) = hub(&root).await;
    let own = To::Own(hub.own_member().await.unwrap());
    let valid = "[cleanup.auto]\nidle_minutes = 30\n";
    write_beside_global_config(&hub.paths, "shared.toml", valid);
    hub.share_round(&daemon, &own, false).await.unwrap();
    assert_eq!(hub_copies(&hub).await, []);
    hub.share_round(&daemon, &own, true).await.unwrap();
    assert_eq!(
        hub_copies(&hub).await,
        [copy(FileName::Config, Some(valid), 1)]
    );
}
