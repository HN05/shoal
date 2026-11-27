use super::*;
use crate::execution::recovery::LastSession;
use crate::stand_in::Fixture;
use shoal_store::test_support::seeds;

/// Queue the restore of `execution` in the fixture's space, `starting`
/// or waiting, its last session kept by a conversion when `converted`.
async fn queue_restore(fixture: &Fixture, execution: &str, starting: bool, converted: bool) {
    let last = LastSession {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        execution: execution.into(),
        agent: "claude".into(),
        session: Some("s1".into()),
        moved: false,
        copied: None,
        origin: None,
        stop: None,
        turn: None,
        converted,
        handoff: None,
    };
    last.write(&fixture.agents.paths, &SpaceId::from("workspace"))
        .unwrap();
    let state = if starting { "starting" } else { "queued" };
    let execution = execution.to_owned();
    (fixture.agents.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,
                     execution_id,state,created_at,seen_at,space_id)
                 VALUES (?1,'restore','repo','work','claude',NULL,'{}',?1,?2,0,0,'workspace')",
                params![execution, state],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_conversions_or_a_starting_restore_moves_the_agent() {
    let fixture = Fixture::new().await;
    (fixture.agents.store)
        .run(|db| {
            seeds::ready_workspace(db, "repo", "workspace", "work")?;
            Ok(())
        })
        .await
        .unwrap();
    let space = SpaceId::from("workspace");
    let moves = || fixture.agents.restores_moved_agent(&space);

    // A restore protection queued waits its turn.
    queue_restore(&fixture, "protected", false, false).await;
    assert!(!moves().await.unwrap());

    queue_restore(&fixture, "converted", false, true).await;
    assert!(moves().await.unwrap());

    (fixture.agents.store)
        .run(|db| Ok(db.execute("DELETE FROM agent_queue WHERE id='converted'", [])?))
        .await
        .unwrap();
    queue_restore(&fixture, "starting", true, false).await;
    assert!(moves().await.unwrap());
}
