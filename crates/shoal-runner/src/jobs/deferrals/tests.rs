use std::{collections::HashSet, time::Duration};

use super::STEP;
use crate::stand_in::Fixture;

/// The machines ranked ahead of this one for the job `h1`, once the
/// runner first saw it with this machine at `rank`.
async fn deferred_at(fixture: &Fixture, rank: Result<usize, String>) -> Option<usize> {
    *fixture.daemon.rank.lock().unwrap() = rank;
    let daemon = fixture.daemon();
    let repository = &fixture.daemon.repository;
    fixture
        .runner
        .defer_by_rank(&*daemon, repository, "h1")
        .await;
    (fixture.runner.jobs.deferrals).ahead(&repository.id, "h1")
}

#[tokio::test(start_paused = true)]
async fn a_machine_ranked_behind_claims_once_a_step_passes_for_each_machine_ahead() {
    let fixture = Fixture::new("").await;
    let deferrals = &fixture.runner.jobs.deferrals;
    // An `always_on` machine, behind an `intermittent` one with room.
    assert_eq!(deferred_at(&fixture, Ok(1)).await, Some(1));
    tokio::time::advance(STEP - Duration::from_secs(1)).await;
    assert_eq!(deferrals.ahead(fixture.repository(), "h1"), Some(1));
    assert!(!deferrals.take_lapsed());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(deferrals.ahead(fixture.repository(), "h1"), None);
    assert!(deferrals.take_lapsed());
    assert!(!deferrals.take_lapsed());
}

#[tokio::test(start_paused = true)]
async fn a_claim_waits_a_step_for_each_machine_ahead() {
    let fixture = Fixture::new("").await;
    assert_eq!(deferred_at(&fixture, Ok(3)).await, Some(3));
    tokio::time::advance(STEP * 3 - Duration::from_secs(1)).await;
    let deferrals = &fixture.runner.jobs.deferrals;
    assert_eq!(deferrals.ahead(fixture.repository(), "h1"), Some(3));
}

#[tokio::test(start_paused = true)]
async fn the_best_ranked_machine_claims_at_once() {
    let fixture = Fixture::new("").await;
    assert_eq!(deferred_at(&fixture, Ok(0)).await, None);
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_cannot_reach_the_hub_claims_at_once() {
    let fixture = Fixture::new("").await;
    let unreachable = Err("the hub is not connected".to_owned());
    assert_eq!(deferred_at(&fixture, unreachable).await, None);
}

#[tokio::test(start_paused = true)]
async fn a_job_the_forge_no_longer_lists_waits_for_no_one() {
    let fixture = Fixture::new("").await;
    deferred_at(&fixture, Ok(1)).await;
    let deferrals = &fixture.runner.jobs.deferrals;
    deferrals.keep_listed(fixture.repository(), &HashSet::from(["h1".to_owned()]));
    assert_eq!(deferrals.ahead(fixture.repository(), "h1"), Some(1));
    deferrals.keep_listed(fixture.repository(), &HashSet::new());
    assert_eq!(deferrals.ahead(fixture.repository(), "h1"), None);
}
