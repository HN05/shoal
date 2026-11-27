use super::*;
use crate::stand_in::Fixture;

#[tokio::test]
async fn the_last_sequence_number_holds_after_the_newest_entry_leaves() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository().await;
    let id = repo.id.clone();
    let (before, after) = (fixture.agents.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,state,created_at)
                 VALUES ('gone','launch',?1,'gone','claude','{}','{}','queued',10)",
                [&id],
            )?;
            let before = last_seq(db)?;
            db.execute("DELETE FROM agent_queue WHERE id='gone'", [])?;
            Ok((before, last_seq(db)?))
        })
        .await
        .unwrap();
    assert!(before > 0);
    assert_eq!(after, before);
}

#[tokio::test]
async fn the_waiting_launches_are_read_with_their_issues_labels() {
    let fixture = Fixture::new().await;
    crate::queue::tests::limit_globally(&fixture, 1);
    let labelled = async |name: &str| {
        let request = QueueRequest {
            issue_labels: vec![format!("area/{name}")],
            ..crate::queue::tests::request(&fixture, name, false).await
        };
        let admission = fixture.agents.queue_add(&*fixture.daemon, request).await;
        admission.unwrap().entry.unwrap()
    };
    let started = labelled("started").await;
    let waiting = labelled("waiting").await;
    assert_eq!(started.state, QueueState::Starting);

    let read = fixture.agents.store.run(|db| labelled_launches(db));
    let launch = LabelledLaunch {
        id: waiting.id,
        repository: waiting.repository_id.unwrap(),
        labels: vec!["area/waiting".into()],
    };
    assert_eq!(read.await.unwrap(), [launch]);
}
