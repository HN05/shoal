use serde_json::json;

use super::*;
use crate::{packages::tests::hub, stand_in::StandIn};

/// The job `rust` of a run, which `laptop`'s sleep cut off at `stopped_at`.
fn cut_off(stopped_at: UnixTime) -> CutOffJob {
    serde_json::from_value(json!({
        "run": "https://forge.example/team/repo/actions/runs/7",
        "job": 12,
        "name": "rust",
        "cut_off_by": "the machine going to sleep",
        "stopped_at": stopped_at,
    }))
    .unwrap()
}

/// The names of the jobs the hub keeps to rerun.
async fn held(hub: &Hub) -> Vec<String> {
    let held = hub.store.read(held_jobs).await.unwrap();
    held.into_iter().map(|held| held.job.name).collect()
}

/// The messages of the user's alerts of `kind`.
async fn alerts(hub: &Hub, kind: AlertKind) -> Vec<String> {
    let alerts = hub.notifications.list(false, 50).await.unwrap();
    (alerts.into_iter())
        .filter(|alert| alert.kind == kind)
        .map(|alert| alert.message)
        .collect()
}

#[tokio::test]
async fn a_handed_job_reruns_once_its_run_finishes_and_the_user_hears_of_it() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    let laptop = MemberId::from("laptop".to_owned());
    let now = UnixTime::now();
    hub.hear_cut_off(&laptop, vec![cut_off(now)]).await.unwrap();
    let daemon = StandIn::default();
    daemon.rerun_finds(CutOffRerun::Unfinished);
    hub.rerun_cut_off_jobs(&daemon, now).await.unwrap();
    assert_eq!(held(&hub).await, ["rust"]);
    assert!(alerts(&hub, AlertKind::JobsRerun).await.is_empty());
    daemon.rerun_finds(CutOffRerun::Rerun);
    hub.rerun_cut_off_jobs(&daemon, now).await.unwrap();
    assert!(held(&hub).await.is_empty());
    let reran = alerts(&hub, AlertKind::JobsRerun).await;
    assert_eq!(reran.len(), 1);
    assert!(reran[0].contains("rust of https://forge.example/team/repo/actions/runs/7"));
    assert!(reran[0].contains("machine laptop"));
}

#[tokio::test]
async fn a_job_no_machine_can_rerun_waits_until_it_is_given_up() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    let laptop = MemberId::from("laptop".to_owned());
    let stopped = UnixTime::now();
    hub.hear_cut_off(&laptop, vec![cut_off(stopped)])
        .await
        .unwrap();
    let daemon = StandIn::default();
    hub.rerun_cut_off_jobs(&daemon, stopped).await.unwrap();
    assert_eq!(held(&hub).await, ["rust"]);
    hub.rerun_cut_off_jobs(&daemon, stopped + PATIENCE)
        .await
        .unwrap();
    assert!(held(&hub).await.is_empty());
    assert_eq!(alerts(&hub, AlertKind::RunnerFailed).await.len(), 1);
    assert!(alerts(&hub, AlertKind::JobsRerun).await.is_empty());
}

#[tokio::test]
async fn a_job_that_needs_no_rerun_goes_without_a_notice() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    let laptop = MemberId::from("laptop".to_owned());
    let now = UnixTime::now();
    hub.hear_cut_off(&laptop, vec![cut_off(now)]).await.unwrap();
    let daemon = StandIn::default();
    daemon.rerun_finds(CutOffRerun::Settled);
    hub.rerun_cut_off_jobs(&daemon, now).await.unwrap();
    assert!(held(&hub).await.is_empty());
    let alerts = hub.notifications.list(false, 50).await.unwrap();
    assert!(alerts.is_empty());
}
