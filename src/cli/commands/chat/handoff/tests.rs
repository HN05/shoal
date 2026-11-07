use super::*;

#[tokio::test]
async fn the_starter_takes_the_places_report() {
    // Two starts at once hand over their tokens apart and take their own
    // reports.
    let started = Handoff::create(Some("first")).unwrap();
    let failed = Handoff::create(Some("second")).unwrap();
    assert_ne!(started.path(), failed.path());
    for (handoff, token) in [(&started, "first"), (&failed, "second")] {
        let handed = std::fs::read_to_string(handoff.path().join(SCOPE)).unwrap();
        assert_eq!(handed, token);
    }
    write_report(started.path(), STARTED);
    write_report(failed.path(), "the agent is not installed");
    started.wait().await.unwrap();
    let error = failed.wait().await.unwrap_err();
    assert_eq!(error.to_string(), "the agent is not installed");
}

#[test]
fn the_handoff_is_private_and_goes_with_its_starter() {
    use std::os::unix::fs::PermissionsExt as _;
    let handoff = Handoff::create(Some("token")).unwrap();
    let mode = std::fs::metadata(handoff.path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    let path = handoff.path().to_owned();
    drop(handoff);
    assert!(!path.exists());
}

#[tokio::test]
async fn an_unscoped_starter_hands_over_no_token_but_takes_the_report() {
    let handoff = Handoff::create(None).unwrap();
    assert_eq!(take_scope(handoff.path()).unwrap(), None);
    write_report(handoff.path(), "the agent is not installed");
    assert!(handoff.wait().await.is_err());
}

#[test]
fn a_place_takes_a_scoped_token_once_and_refuses_a_handoff_its_starter_left() {
    let scoped = Handoff::create(Some("token")).unwrap();
    assert_eq!(take_scope(scoped.path()).unwrap().as_deref(), Some("token"));
    // A starter that stopped waiting removed its handoff, which never
    // starts the chat unscoped instead.
    let path = scoped.path().to_owned();
    drop(scoped);
    assert!(take_scope(&path).is_err());
}

#[tokio::test(start_paused = true)]
async fn the_starter_waits_on_a_running_place_beyond_the_pickup_limit() {
    let handoff = Handoff::create(None).unwrap();
    let path = handoff.path().to_owned();
    // The only test that holds a place, which this process does until it
    // exits.
    hold(&path).unwrap();
    let waiting = tokio::spawn(handoff.wait());
    tokio::time::sleep(PICKUP * 10).await;
    assert!(!waiting.is_finished());
    write_report(&path, STARTED);
    waiting.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_starter_learns_of_a_place_that_ended_without_a_report() {
    let handoff = Handoff::create(None).unwrap();
    // A place that took the handoff and exited left its lock unheld.
    File::create(handoff.path().join(PLACE)).unwrap();
    let error = handoff.wait().await.unwrap_err();
    assert!(error.to_string().contains("ended"), "{error}");
}

#[tokio::test(start_paused = true)]
async fn the_starter_gives_up_on_a_place_that_never_takes_the_handoff() {
    let handoff = Handoff::create(None).unwrap();
    let started = tokio::time::Instant::now();
    let error = handoff.wait().await.unwrap_err();
    assert!(error.to_string().contains("did not run Shoal"), "{error}");
    assert!(started.elapsed() >= PICKUP);
}
