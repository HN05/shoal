use super::*;
use crate::execution::recovery::RecordExt;
use crate::stand_in::Fixture;
use crate::state::AgentState;
use crate::state::TurnFailure;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::agent::Agent;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

/// The turn generation the space `id` records.
async fn turn(fixture: &Fixture, id: &SpaceId) -> i64 {
    let id = id.clone();
    fixture
        .agents
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT turn FROM space_agent_state WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn turn_generations_count_turn_starts_and_continue_after_the_state_goes() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("agent").await;
    let id = workspace.id.clone();
    let execution = |sql: &'static str| {
        let id = id.clone();
        fixture.agents.store.run(move |db| {
            db.execute(sql, [id])?;
            Ok(())
        })
    };
    execution("INSERT INTO executions(id,space_id,state) VALUES ('agent',?1,'running')")
        .await
        .unwrap();
    let selected = SpaceSelector::id(&workspace.id);
    let report = |state, execution: &str| {
        fixture.agents.set_agent_state(
            &*fixture.daemon,
            &selected,
            state,
            Some(execution.into()),
            None,
        )
    };
    report(AgentState::Working, "agent").await.unwrap();
    let first = turn(&fixture, &workspace.id).await;
    for state in [AgentState::Working, AgentState::Idle, AgentState::Waiting] {
        report(state, "agent").await.unwrap();
        assert_eq!(turn(&fixture, &workspace.id).await, first, "{state:?}");
    }
    report(AgentState::Working, "agent").await.unwrap();
    assert_eq!(turn(&fixture, &workspace.id).await, first + 1);

    // The state goes with its execution, and the next one counts on.
    execution("DELETE FROM executions WHERE space_id=?1")
        .await
        .unwrap();
    execution("INSERT INTO executions(id,space_id,state) VALUES ('next',?1,'running')")
        .await
        .unwrap();
    report(AgentState::Idle, "next").await.unwrap();
    assert_eq!(turn(&fixture, &workspace.id).await, first + 2);
}

#[tokio::test]
async fn an_interrupt_stays_with_the_execution_whose_turn_it_ended() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("agent").await;
    let id = workspace.id.clone();
    fixture
        .agents
        .store
        .run(move |db| {
            for execution in ["first", "second"] {
                db.execute(
                    "INSERT INTO executions(id,space_id,state) VALUES (?1,?2,'running')",
                    rusqlite::params![execution, id],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let selected = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .end_interrupted_turn(&*fixture.daemon, &selected, Some("first".into())))
    .await
    .unwrap();
    // A report that says nothing of an interrupt, from another execution.
    (fixture.agents.set_agent_state(
        &*fixture.daemon,
        &selected,
        AgentState::Idle,
        Some("second".into()),
        None,
    ))
    .await
    .unwrap();
    let interrupted = fixture
        .agents
        .store
        .run(|db| interrupted(db, &"second".into()))
        .await
        .unwrap();
    assert!(!interrupted);
}

#[tokio::test]
async fn a_failed_turn_leaves_the_agent_idle() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("agent").await;
    fixture
        .agents
        .set_agent_state(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    let status = fixture
        .agents
        .report_turn_failure(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            TurnFailure::Overloaded,
            None,
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
}

#[tokio::test]
async fn an_ended_turn_starts_the_prompt_cache_lifetime_until_the_agent_works_again() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("agent").await;
    let hour = TurnEnd {
        prompt_cache_secs: Some(3600),
        ..TurnEnd::default()
    };
    let ended = fixture
        .agents
        .end_agent_turn(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            None,
            None,
            hour,
        )
        .await
        .unwrap();
    assert_eq!(ended.state, AgentState::Idle);
    assert_eq!(
        ended.prompt_cache_expires_at,
        Some(ended.since + std::time::Duration::from_secs(3600))
    );
    // Another idle report, such as an idle prompt's, keeps the expiry.
    let repeated = fixture
        .agents
        .set_agent_state(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(repeated, ended);
    // Working drops it, and idle without an ended turn knows none.
    for state in [AgentState::Working, AgentState::Idle] {
        let status = fixture
            .agents
            .set_agent_state(
                &*fixture.daemon,
                &SpaceSelector::id(&workspace.id),
                state,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(status.prompt_cache_expires_at, None, "{state:?}");
    }
    // A turn whose agent knows no lifetime gets the default one. Working
    // first makes the turn's end start a new `since`.
    fixture
        .agents
        .set_agent_state(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    let unknown = fixture
        .agents
        .end_agent_turn(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            None,
            None,
            TurnEnd::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        unknown.prompt_cache_expires_at,
        Some(unknown.since + std::time::Duration::from_secs(1800))
    );
}

#[tokio::test]
async fn the_agents_ai_tool_decides_its_prompt_cache_lifetime() {
    let fixture = Fixture::with_config("[agents.claude]\nprompt_cache_minutes = 5\n").await;
    let workspace = fixture.workspace("agent").await;
    let claude = Agent::from("claude");
    let execution = fixture.execution(&workspace, "agent", Some(claude)).await;
    let reported = TurnEnd {
        prompt_cache_secs: Some(3600),
        ..TurnEnd::default()
    };
    let space = SpaceSelector::id(&workspace.id);
    let ended = (fixture.agents)
        .end_agent_turn(&*fixture.daemon, &space, Some(execution), None, reported)
        .await
        .unwrap();
    assert_eq!(
        ended.prompt_cache_expires_at,
        Some(ended.since + std::time::Duration::from_secs(300))
    );
}

#[tokio::test]
async fn a_chat_stopped_or_removed_before_the_write_takes_no_report() {
    let fixture = Fixture::new().await;
    let chat = fixture.chat("talk").await;
    // The report was checked against the running chat; the stop, then the
    // removal, commit before it is written.
    for ended in [
        "DELETE FROM executions WHERE space_id=?1",
        "DELETE FROM spaces WHERE id=?1",
    ] {
        let id = chat.clone();
        let records = fixture.agents.records.clone();
        let saved = fixture
            .agents
            .store
            .write(move |tx| {
                tx.execute(ended, [&id])?;
                let report = Report {
                    space: id.clone(),
                    state: AgentState::Working,
                    execution: None,
                    at: UnixTime::from_seconds(1),
                    prompt_cache_expires_at: None,
                    failed: false,
                    interrupted: None,
                    woken: false,
                };
                Ok((save(tx, &*records, &report)?, get(tx, &id)?))
            })
            .await
            .unwrap();
        assert_eq!(saved, (None, None), "{ended}");
    }
}

async fn workspace(fixture: &Arc<Fixture>) -> crate::model::Workspace {
    fixture.workspace("agent").await
}

#[tokio::test]
async fn a_forgotten_execution_keeps_its_turn_in_the_record_it_saved() {
    use crate::execution::recovery::{Record, record_path, save_record};
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
    let id = workspace.id.clone();
    fixture
        .agents
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES
                    ('stopped',?1,'unknown'),('exited',?1,'unknown')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    save_record(
        &fixture.agents.paths,
        &workspace.id,
        &ExecutionId::from("stopped"),
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    for execution in [ExecutionId::from("stopped"), ExecutionId::from("exited")] {
        fixture
            .agents
            .set_agent_state(
                &*fixture.daemon,
                &SpaceSelector::id(&workspace.id),
                AgentState::Idle,
                Some(execution.clone()),
                None,
            )
            .await
            .unwrap();
        fixture
            .agents
            .forget_execution(&*fixture.daemon, &execution)
            .await
            .unwrap();
    }

    let path = record_path(
        &fixture.agents.paths,
        &workspace.id,
        &ExecutionId::from("stopped"),
    );
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.turn, Some(AgentState::Idle));
    // An execution that saved no record stopped nothing to keep a turn for.
    assert!(
        !record_path(
            &fixture.agents.paths,
            &workspace.id,
            &ExecutionId::from("exited")
        )
        .exists(),
        "a turn alone must not make a recovery record"
    );
}
