use rusqlite::params;

use super::*;
use crate::queue::{Serving, last_seq};
use crate::stand_in::Fixture;
use crate::state::ExecutionState;

#[tokio::test]
async fn a_review_arrives_after_the_entries_queued_before_it_in_the_same_second() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository().await;
    let id = repo.id.clone();
    let queued_after = (fixture.agents.store)
        .run(move |db| {
            let insert = |entry: &str| {
                db.execute(
                    "INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,state,created_at)
                     VALUES (?1,'launch',?2,?1,'claude','{}','{}','queued',10)",
                    params![entry, id],
                )
            };
            insert("early")?;
            let queued_after = last_seq(db)?;
            insert("late")?;
            Ok(queued_after)
        })
        .await
        .unwrap();
    let review = WaitingOneShot {
        id: String::from("oneshot:0").into(),
        repository: repo.id.clone(),
        serving: Some(Serving {
            space: String::from("reviewed").into(),
            queued_after,
        }),
        requested_at: UnixTime::from_seconds(10),
        after_stop: false,
    };
    let entries = (fixture.agents.store)
        .read(move |tx| waiting(tx, &HashMap::new(), &[review]))
        .await
        .unwrap();
    let arrived: Vec<_> = (entries.iter())
        .map(|entry| (entry.id.to_string(), entry.arrival))
        .collect();
    assert_eq!(
        arrived,
        [
            ("early".into(), 0),
            ("oneshot:0".into(), 1),
            ("late".into(), 2)
        ]
    );
}

#[tokio::test]
async fn a_queued_restore_reads_when_its_agents_prompt_cache_expires() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("stopped").await;
    let id = workspace.id.clone();
    let entries = (fixture.agents.store)
        .read(move |tx| {
            tx.execute(
                "INSERT INTO executions(id,space_id,state,agent) VALUES ('agent',?1,?2,'claude')",
                params![id, ExecutionState::Unknown],
            )?;
            tx.execute(
                "INSERT INTO space_agent_state(space_id,state,execution_id,since,prompt_cache_expires_at)
                 VALUES (?1,'idle','agent',1,500)",
                [&id],
            )?;
            tx.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,space_id,execution_id)
                 VALUES ('restore','restore','repo','stopped','claude',?1,1,?2,'agent')",
                params![QueueState::Queued, id],
            )?;
            waiting(tx, &Default::default(), &[])
        })
        .await
        .unwrap();
    let caches: Vec<_> = entries
        .iter()
        .map(|entry| entry.prompt_cache_until)
        .collect();
    assert_eq!(caches, [Some(UnixTime::from_seconds(500))]);
}
