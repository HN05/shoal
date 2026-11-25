use shoal_protocol::progress::Blocker;

use super::*;

/// Queue `name` blocked by `blockers`.
async fn add_blocked(fixture: &Fixture, name: &str, blockers: Vec<Blocker>) -> QueueEntry {
    let request = QueueRequest {
        blockers,
        ..request(fixture, name, false).await
    };
    let admission = fixture.agents.queue_add(&*fixture.daemon, request).await;
    let admission = admission.unwrap();
    assert!(!admitted(&admission), "{name} waits for its blockers");
    admission.entry.unwrap()
}

async fn entry(fixture: &Fixture, id: &QueueEntryId) -> QueueEntry {
    (fixture.agents.queue_entries(&*fixture.daemon))
        .await
        .unwrap()
        .into_iter()
        .find(|entry| &entry.id == id)
        .unwrap()
}

async fn space(fixture: &Fixture, name: &str) -> Blocker {
    Blocker::Space(queued(fixture, name).await)
}

#[tokio::test]
async fn a_launch_blocker_follows_the_workspace_it_opens_until_removal() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let opened = fixture.workspace("first").await;
    let first = fixture
        .agents
        .queue_add(daemon, launch_in(&opened, false))
        .await;
    let first = first.unwrap().entry.unwrap();
    let second = add_blocked(&fixture, "second", vec![space(&fixture, "first").await]).await;
    assert_eq!(second.blockers, ["first"]);

    (fixture.agents)
        .queue_attach(daemon, &first.id, &SpaceSelector::id(&opened.id))
        .await
        .unwrap();
    assert_eq!(entry(&fixture, &second.id).await.blockers, ["first"]);
    // A removed blocker leaves nothing to wait for, so the launch starts
    // from the default branch in the slot the removal freed.
    fixture.remove(&opened.id).await;
    dispatch(&fixture).await;
    assert!(started(&second.id));
    assert_eq!(entry(&fixture, &second.id).await.base, None);
}

#[tokio::test]
async fn blockers_name_existing_work_other_than_the_launch() {
    let fixture = Fixture::new().await;
    fixture
        .configure("[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false");
    let daemon = &*fixture.daemon;
    let repo = fixture.repository().await;
    // A selector names a space by a UUID.
    let workspace = async |name| {
        let id = SpaceId::generate();
        (fixture.workspace_with_id(&repo, id.as_str(), name)).await
    };
    let blockers = [
        space(&fixture, "missing").await,
        space(&fixture, "self").await,
    ];
    let own = workspace("self").await;
    for blocker in blockers {
        let request = QueueRequest {
            blockers: vec![blocker],
            ..launch_in(&own, false)
        };
        fixture.agents.queue_add(daemon, request).await.unwrap_err();
    }
    assert!(live(&fixture).await.is_empty());
    // The CLI checks them before any prompt.
    let selector = crate::repository::Selector::Id(own.repository().clone());
    let check = async |name| {
        let blockers = vec![space(&fixture, name).await];
        fixture
            .agents
            .check_blockers(daemon, &selector, blockers)
            .await
    };
    check("missing").await.unwrap_err();
    workspace("present").await;
    check("present").await.unwrap();
    // Starting now cannot wait for work that is not done.
    let request = QueueRequest {
        blockers: vec![space(&fixture, "present").await],
        ..request(&fixture, "now", true).await
    };
    fixture.agents.queue_add(daemon, request).await.unwrap_err();
}
