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

#[tokio::test]
async fn a_launch_queues_the_agent_of_the_workspace_it_opens_alone() {
    let fixture = Fixture::new().await;
    let other = fixture.workspace("other").await;
    let launched = crate::queue::tests::add(&fixture, "launched").await;
    let launched = launched.entry.unwrap().space().unwrap().clone();

    let queues = fixture
        .agents
        .store
        .run(move |db| Ok([launched, other.id].map(|space| queues_agent_of(db, &space).unwrap())));
    assert_eq!(queues.await.unwrap(), [true, false]);
}

#[tokio::test]
async fn a_waiting_job_queues_in_its_repository_without_holding_work_there() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository().await.id;
    let other = fixture.repository_named("other").await.id;
    let job = serde_json::from_value(serde_json::json!({
        "job": {"handle": "h", "id": 1, "run": 1, "name": "rust", "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/1", "workflow": "ci.yml", "commit": "abc", "trigger": "other"},
    }))
    .unwrap();
    let read = fixture.agents.store.run(move |db| {
        crate::queue::jobs::queue(db, &repo, &job)?;
        let read = |repository| {
            Ok::<_, anyhow::Error>((queues_in(db, repository)?, queues_work_in(db, repository)?))
        };
        let waiting = [read(&repo)?, read(&other)?];
        db.execute("UPDATE agent_queue SET state=?1", [QueueState::Starting])?;
        Ok((waiting, read(&repo)?))
    });
    let (waiting, admitted) = read.await.unwrap();
    assert_eq!(waiting, [(true, false), (false, false)]);
    assert_eq!(admitted, (true, true));
}
