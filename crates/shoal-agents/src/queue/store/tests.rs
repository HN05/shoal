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
