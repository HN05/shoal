use super::*;
use crate::client::space_ids::SpaceId;

fn pool() -> Target {
    Target::Resource {
        scope: crate::daemon::resources::Scope::Global,
        pool: "devices".into(),
    }
}

fn busy() -> Result<Allocation<()>, ()> {
    Ok(Allocation::Busy("full".into()))
}

/// Poll once and stay queued, as a waiting command that found no capacity.
fn wait(queues: &Queues, workspace: &SpaceId) -> usize {
    let mut place = queues.enter(pool(), workspace, "default", true);
    let ahead = place.ahead();
    place.settle(&busy());
    ahead
}

#[tokio::test(start_paused = true)]
async fn waiters_are_served_in_arrival_order() {
    let queues = Queues::default();
    assert_eq!(wait(&queues, &SpaceId::from("first")), 0);
    assert_eq!(wait(&queues, &SpaceId::from("second")), 1);
    assert_eq!(
        wait(&queues, &SpaceId::from("first")),
        0,
        "polling again keeps the place"
    );
    let outside = queues.enter(pool(), &SpaceId::from("other"), "default", false);
    assert_eq!(outside.ahead(), 2);
    assert!(matches!(
        behind::<()>(outside.ahead(), "pool devices"),
        Some(Allocation::Busy(message)) if message == "2 earlier requests are queued for pool devices"
    ));
    drop(outside);
    assert_eq!(
        wait(&queues, &SpaceId::from("second")),
        1,
        "outside requests take no place"
    );

    let mut first = queues.enter(pool(), &SpaceId::from("first"), "default", true);
    assert!(behind::<()>(first.ahead(), "pool devices").is_none());
    first.settle(&Ok::<_, ()>(Allocation::Granted(())));
    drop(first);
    assert_eq!(
        wait(&queues, &SpaceId::from("second")),
        0,
        "a grant leaves the queue"
    );
    let mut errored = queues.enter(pool(), &SpaceId::from("second"), "default", true);
    errored.settle(&Err::<Allocation<()>, _>(()));
    drop(errored);
    assert!(queues.0.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn abandoned_waiters_lose_their_place_unless_in_progress() {
    let queues = Queues::default();
    wait(&queues, &SpaceId::from("gone"));
    let in_progress = queues.enter(pool(), &SpaceId::from("booting"), "default", true);
    tokio::time::advance(ABANDONED_AFTER).await;
    assert_eq!(
        wait(&queues, &SpaceId::from("late")),
        1,
        "only the request in progress remains"
    );
    drop(in_progress);
    assert_eq!(wait(&queues, &SpaceId::from("late")), 0);
}

#[tokio::test(start_paused = true)]
async fn targets_and_lease_names_queue_independently() {
    let queues = Queues::default();
    wait(&queues, &SpaceId::from("first"));
    let other_pool = queues.enter(
        Target::Simulator,
        &SpaceId::from("second"),
        "default",
        false,
    );
    assert_eq!(other_pool.ahead(), 0);
    drop(other_pool);
    let mut tests = queues.enter(pool(), &SpaceId::from("first"), "tests", true);
    assert_eq!(tests.ahead(), 1);
    tests.settle(&busy());
}
