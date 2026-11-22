use super::*;
use crate::{
    agents::hook_reports::Sending,
    model::{AgentStatus, Workspace},
    protocol::TurnEnd,
    stand_in::Fixture,
    state::ExecutionState,
};
use shoal_core::paths::StateFiles;
use shoal_protocol::agent::Agent;

/// A workspace whose agent runs in the execution `agent`, in `state`.
async fn workspace_in(fixture: &Fixture, state: ExecutionState) -> Workspace {
    let workspace = fixture.workspace("reporter").await;
    let id = workspace.id.clone();
    (fixture.agents.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('agent',?1,?2)",
                rusqlite::params![id, state],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    workspace
}

async fn workspace(fixture: &Fixture) -> Workspace {
    workspace_in(fixture, ExecutionState::Running).await
}

fn ended_turn() -> Report {
    Report::State {
        state: AgentState::Idle,
        session: None,
        turn_end: Some(TurnEnd::default()),
    }
}

async fn report(fixture: &Fixture, workspace: &Workspace, state: AgentState) {
    let selector = SpaceSelector::id(&workspace.id);
    fixture
        .agents
        .set_agent_state(
            &*fixture.daemon,
            &selector,
            state,
            Some("agent".into()),
            None,
        )
        .await
        .unwrap();
}

/// A report the hook of `workspace` could not send for `error`.
fn lose(fixture: &Fixture, workspace: &Workspace, report: Report, error: &str) {
    Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        report,
        Some("agent".into()),
    )
    .unwrap()
    .failed(error.into());
}

async fn state(fixture: &Fixture, workspace: &Workspace) -> Option<AgentStatus> {
    let id = workspace.id.clone();
    fixture
        .agents
        .store
        .run(move |db| crate::turn_state::current(db, &id))
        .await
        .unwrap()
}

/// The space each failed state report named, with its error.
async fn failures(fixture: &Fixture) -> Vec<(Option<String>, String)> {
    let failures = fixture.daemon.state_failures.lock().unwrap();
    (failures.iter())
        .map(|(selector, error)| (selector.given_id().map(|id| id.to_string()), error.clone()))
        .collect()
}

#[tokio::test]
async fn a_lost_report_alerts_and_arrives_late() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    lose(
        &fixture,
        &workspace,
        ended_turn(),
        "daemon protocol mismatch",
    );

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    let shown = state(&fixture, &workspace).await.unwrap();
    assert_eq!(shown.state, AgentState::Idle);
    // A turn's end tells how long the prompt cache lives.
    assert!(shown.prompt_cache_expires_at.is_some());
    let failures = failures(&fixture).await;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].0.as_deref(), Some("reporter"));
    assert!(failures[0].1.contains("protocol mismatch"), "{failures:?}");
}

#[tokio::test]
async fn a_lost_report_a_later_one_overtook_only_alerts() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    lose(&fixture, &workspace, ended_turn(), "timed out");
    // Repeating the state keeps its start, so only the arrival tells.
    report(&fixture, &workspace, AgentState::Working).await;

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Working
    );
    assert_eq!(failures(&fixture).await.len(), 1);
}

#[tokio::test]
async fn a_lost_report_of_an_ended_execution_only_alerts() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    let waiting = Report::State {
        state: AgentState::Waiting,
        session: None,
        turn_end: None,
    };
    Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        waiting,
        Some("ended".into()),
    )
    .unwrap()
    .failed("timed out".into());

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Working
    );
    assert_eq!(failures(&fixture).await.len(), 1);
}

#[tokio::test]
async fn the_reports_of_a_removed_space_go_with_it() {
    let fixture = Fixture::new().await;
    let gone = SpaceId::from("gone".to_owned());
    Sending::keep(&fixture.agents.paths, &gone, ended_turn(), None)
        .unwrap()
        .failed("timed out".into());

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert!(failures(&fixture).await.is_empty());
    assert!(
        hook_reports::spaces(&fixture.agents.paths)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_lost_report_waits_for_its_execution_to_reattach() {
    let fixture = Fixture::new().await;
    let workspace = workspace_in(&fixture, ExecutionState::Unknown).await;
    lose(&fixture, &workspace, ended_turn(), "timed out");

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;
    assert!(failures(&fixture).await.is_empty());
    let id = workspace.id.clone();
    fixture
        .agents
        .store
        .run(move |db| {
            db.execute(
                "UPDATE executions SET state='running' WHERE space_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(failures(&fixture).await.len(), 1);
    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_report_delivered_before_a_restart_overtakes_a_lost_one() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    lose(&fixture, &workspace, ended_turn(), "timed out");
    // Delivered to a daemon before this one, whose arrivals it does not know.
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        working,
        Some("agent".into()),
    )
    .unwrap()
    .delivered();

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(state(&fixture, &workspace).await, None);
    assert_eq!(failures(&fixture).await.len(), 1);
}

#[tokio::test]
async fn a_lost_report_waits_for_its_agent_to_register() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    // The wrapper reattached, but has yet to register the agent it runs.
    fixture
        .agents
        .store
        .run(|db| {
            db.execute("UPDATE executions SET agent='claude' WHERE id='agent'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let failed = Report::TurnFailure {
        failure: crate::state::TurnFailure::Overloaded,
        session: None,
        turn_end: TurnEnd::default(),
    };
    lose(&fixture, &workspace, failed, "timed out");

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert!(failures(&fixture).await.is_empty());
    assert_eq!(state(&fixture, &workspace).await, None);
    assert_eq!(
        hook_reports::find(&fixture.agents.paths, &workspace.id)
            .unwrap()
            .left
            .len(),
        1
    );
}

#[tokio::test]
async fn a_newer_lost_report_arrives_after_an_older_one_was_recovered() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    lose(&fixture, &workspace, working, "timed out");
    // The turn's end, whose hook still waits on the daemon.
    let sending = Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        ended_turn(),
        Some("agent".into()),
    )
    .unwrap();

    // The earlier report waits for the later one, which may overtake it.
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;
    assert_eq!(state(&fixture, &workspace).await, None);
    drop(sending);
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_lost_report_whose_record_failed_is_recovered_on_the_next_pass() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    lose(&fixture, &workspace, ended_turn(), "timed out");
    let fail_writes = |sql: &'static str| {
        let fixture = &fixture;
        async move {
            fixture
                .agents
                .store
                .run(move |db| {
                    db.execute_batch(sql)?;
                    Ok(())
                })
                .await
                .unwrap()
        }
    };
    fail_writes(
        "CREATE TRIGGER full BEFORE INSERT ON space_agent_state
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
    )
    .await;

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;
    fail_writes("DROP TRIGGER full").await;
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_lost_report_whose_mark_fails_is_recorded_on_the_next_pass() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    lose(&fixture, &workspace, ended_turn(), "timed out");
    let mark = fixture
        .agents
        .paths
        .hook_reports()
        .join(&workspace.id)
        .join(".delivered");
    std::fs::create_dir(&mark).unwrap();

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;
    std::fs::remove_dir(&mark).unwrap();
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Idle
    );
    assert!(hook_reports::last_delivered(&fixture.agents.paths, &workspace.id).is_some());
}

#[tokio::test]
async fn reports_are_ordered_by_when_their_hooks_made_them() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    let made = SystemTime::now();
    // The turn's end, made after the report that started its work, which
    // a stalled daemon recorded only afterwards.
    lose(&fixture, &workspace, ended_turn(), "timed out");
    fixture
        .agents
        .report_agent_state(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            AgentState::Working,
            Some("agent".into()),
            None,
            None,
            Some(made),
        )
        .await
        .unwrap();

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_live_report_made_before_a_recorded_one_changes_nothing() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    let made = SystemTime::now();
    lose(&fixture, &workspace, ended_turn(), "timed out");
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    // A report made before the turn's end, which a stalled daemon answers
    // only now.
    let status = fixture
        .agents
        .report_agent_state(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            AgentState::Working,
            Some("agent".into()),
            None,
            None,
            Some(made),
        )
        .await
        .unwrap();

    assert_eq!(status.state, AgentState::Idle);
    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_failed_turn_lost_before_a_later_idle_report_is_still_retried() {
    let fixture = Fixture::with_config("[agents.claude]\nretry = true\n").await;
    let workspace = fixture.workspace("reporter").await;
    let claude = Agent::from("claude");
    let started = fixture.execution(&workspace, "agent", Some(claude)).await;
    let execution = Some(started);
    let lose_from = |report: Report| {
        Sending::keep(
            &fixture.agents.paths,
            &workspace.id,
            report,
            execution.clone(),
        )
        .unwrap()
        .failed("timed out".into());
    };
    lose_from(Report::TurnFailure {
        failure: crate::state::TurnFailure::Overloaded,
        session: None,
        turn_end: TurnEnd::default(),
    });
    // The notice that the agent's prompt waits for input, which follows.
    lose_from(Report::State {
        state: AgentState::Idle,
        session: None,
        turn_end: None,
    });

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    let id = workspace.id.clone();
    let retry = fixture
        .agents
        .store
        .run(move |db| crate::turn_retry::get(db, &id))
        .await
        .unwrap();
    assert!(retry.is_some(), "the failed turn is retried");
    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_lost_report_waits_for_a_later_one_its_hook_still_sends() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    lose(&fixture, &workspace, ended_turn(), "timed out");
    // A later report the daemon recorded, whose hook has yet to mark it
    // delivered when the daemon restarts.
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    let answered = Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        working,
        Some("agent".into()),
    )
    .unwrap();
    let fixture = fixture.restart().await;

    fixture.agents.recover_lost_reports(&*fixture.daemon).await;
    assert!(failures(&fixture).await.is_empty());
    answered.delivered();
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Working
    );
    assert_eq!(failures(&fixture).await.len(), 1);
}

#[tokio::test]
async fn a_delayed_live_report_waits_behind_a_later_one_still_sent() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    let made = SystemTime::now();
    // A later report the daemon recorded, whose hook has yet to mark it
    // delivered when the daemon restarts.
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    let answered = Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        working,
        Some("agent".into()),
    )
    .unwrap();
    let fixture = fixture.restart().await;

    // The earlier turn's end, which reaches the new daemon only now.
    fixture
        .agents
        .report_agent_state(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            Some("agent".into()),
            None,
            Some(TurnEnd::default()),
            Some(made),
        )
        .await
        .unwrap();
    answered.delivered();

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Working
    );
}

#[tokio::test]
async fn a_late_turns_prompt_cache_lives_from_when_the_turn_ended() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    let ended = UnixTime::now() - Duration::from_secs(120);
    let turn_end = TurnEnd {
        prompt_cache_secs: Some(300),
        ..TurnEnd::default()
    };

    let status = fixture
        .agents
        .report_agent_state(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            None,
            None,
            Some(turn_end),
            Some(ended.system_time()),
        )
        .await
        .unwrap();

    assert_eq!(status.since, ended);
    assert_eq!(
        status.prompt_cache_expires_at,
        Some(ended + Duration::from_secs(300))
    );
}

#[tokio::test]
async fn a_recovered_report_outlasts_a_restart() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    // An earlier turn's end whose hook still waits on the daemon.
    let earlier = Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        ended_turn(),
        Some("agent".into()),
    )
    .unwrap();
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    lose(&fixture, &workspace, working, "timed out");
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;
    drop(earlier);

    let fixture = fixture.restart().await;
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Working
    );
}

#[tokio::test]
async fn a_report_whose_hook_died_after_its_answer_still_orders_a_restart() {
    let fixture = Fixture::new().await;
    let workspace = workspace(&fixture).await;
    report(&fixture, &workspace, AgentState::Working).await;
    // An earlier turn's end whose hook still waits on the daemon.
    let earlier = Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        ended_turn(),
        Some("agent".into()),
    )
    .unwrap();
    // A later report the daemon recorded, whose hook ended before it
    // could mark it delivered.
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    let answered = Sending::keep(
        &fixture.agents.paths,
        &workspace.id,
        working,
        Some("agent".into()),
    )
    .unwrap();
    report(&fixture, &workspace, AgentState::Working).await;
    drop(answered);
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;
    drop(earlier);

    let fixture = fixture.restart().await;
    fixture.agents.recover_lost_reports(&*fixture.daemon).await;

    assert_eq!(
        state(&fixture, &workspace).await.unwrap().state,
        AgentState::Working
    );
}
