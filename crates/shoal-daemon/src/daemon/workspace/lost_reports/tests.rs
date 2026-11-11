use super::*;
use crate::{
    agents::hook_reports::Sending,
    client::notifications::NotificationKind,
    client::space_ids::SpaceId,
    daemon::test_support::{manager, repository},
    model::{AgentStatus, Workspace},
    protocol::TurnEnd,
    state::ExecutionState,
};

/// A workspace whose agent runs in the execution `agent`, in `state`.
async fn workspace_in(
    manager: &Manager,
    temp: &tempfile::TempDir,
    state: ExecutionState,
) -> Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "reporter".into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state) VALUES ('agent',?1,?2)",
                rusqlite::params![id, state],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    workspace
}

async fn workspace(manager: &Manager, temp: &tempfile::TempDir) -> Workspace {
    workspace_in(manager, temp, ExecutionState::Running).await
}

fn ended_turn() -> Report {
    Report::State {
        state: AgentState::Idle,
        session: None,
        turn_end: Some(TurnEnd::default()),
    }
}

async fn report(manager: &Manager, workspace: &Workspace, state: AgentState) {
    let selector = SpaceSelector::id(&workspace.id);
    manager
        .set_agent_state(&selector, state, Some("agent".into()), None)
        .await
        .unwrap();
}

/// A report the hook of `workspace` could not send for `error`.
fn lose(manager: &Manager, workspace: &Workspace, report: Report, error: &str) {
    Sending::keep(&manager.paths, &workspace.id, report, Some("agent".into()))
        .unwrap()
        .failed(error.into());
}

async fn state(manager: &Manager, workspace: &Workspace) -> Option<AgentStatus> {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| crate::daemon::workspace::agent_state::current(db, &id))
        .await
        .unwrap()
}

async fn failures(manager: &Manager) -> Vec<(Option<String>, String)> {
    manager
        .notifications(true, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|notification| notification.kind == NotificationKind::StateReportFailed)
        .map(|notification| {
            let workspace = notification.workspace.map(|name| name.to_string());
            (workspace, notification.message)
        })
        .collect()
}

#[tokio::test]
async fn a_lost_report_alerts_and_arrives_late() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    lose(
        &manager,
        &workspace,
        ended_turn(),
        "daemon protocol mismatch",
    );

    manager.recover_lost_reports().await;

    let shown = state(&manager, &workspace).await.unwrap();
    assert_eq!(shown.state, AgentState::Idle);
    // A turn's end tells how long the prompt cache lives.
    assert!(shown.prompt_cache_expires_at.is_some());
    let failures = failures(&manager).await;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].0.as_deref(), Some("reporter"));
    assert!(failures[0].1.contains("protocol mismatch"), "{failures:?}");
}

#[tokio::test]
async fn a_lost_report_a_later_one_overtook_only_alerts() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    lose(&manager, &workspace, ended_turn(), "timed out");
    // Repeating the state keeps its start, so only the arrival tells.
    report(&manager, &workspace, AgentState::Working).await;

    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Working
    );
    assert_eq!(failures(&manager).await.len(), 1);
}

#[tokio::test]
async fn a_lost_report_of_an_ended_execution_only_alerts() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    let waiting = Report::State {
        state: AgentState::Waiting,
        session: None,
        turn_end: None,
    };
    Sending::keep(&manager.paths, &workspace.id, waiting, Some("ended".into()))
        .unwrap()
        .failed("timed out".into());

    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Working
    );
    assert_eq!(failures(&manager).await.len(), 1);
}

#[tokio::test]
async fn the_reports_of_a_removed_space_go_with_it() {
    let (_temp, manager) = manager().await;
    let gone = SpaceId::from("gone".to_owned());
    Sending::keep(&manager.paths, &gone, ended_turn(), None)
        .unwrap()
        .failed("timed out".into());

    manager.recover_lost_reports().await;

    assert!(failures(&manager).await.is_empty());
    assert!(hook_reports::spaces(&manager.paths).unwrap().is_empty());
}

#[tokio::test]
async fn a_lost_report_waits_for_its_execution_to_reattach() {
    let (temp, manager) = manager().await;
    let workspace = workspace_in(&manager, &temp, ExecutionState::Unknown).await;
    lose(&manager, &workspace, ended_turn(), "timed out");

    manager.recover_lost_reports().await;
    assert!(failures(&manager).await.is_empty());
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE executions SET state='running' WHERE workspace_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.recover_lost_reports().await;

    assert_eq!(failures(&manager).await.len(), 1);
    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_report_delivered_before_a_restart_overtakes_a_lost_one() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    lose(&manager, &workspace, ended_turn(), "timed out");
    // Delivered to a daemon before this one, whose arrivals it does not know.
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    Sending::keep(&manager.paths, &workspace.id, working, Some("agent".into()))
        .unwrap()
        .delivered();

    manager.recover_lost_reports().await;

    assert_eq!(state(&manager, &workspace).await, None);
    assert_eq!(failures(&manager).await.len(), 1);
}

#[tokio::test]
async fn a_lost_report_waits_for_its_agent_to_register() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    // The wrapper reattached, but has yet to register the agent it runs.
    manager
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
    lose(&manager, &workspace, failed, "timed out");

    manager.recover_lost_reports().await;

    assert!(failures(&manager).await.is_empty());
    assert_eq!(state(&manager, &workspace).await, None);
    assert_eq!(
        hook_reports::find(&manager.paths, &workspace.id)
            .unwrap()
            .left
            .len(),
        1
    );
}

#[tokio::test]
async fn a_newer_lost_report_arrives_after_an_older_one_was_recovered() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    lose(&manager, &workspace, working, "timed out");
    // The turn's end, whose hook still waits on the daemon.
    let sending = Sending::keep(
        &manager.paths,
        &workspace.id,
        ended_turn(),
        Some("agent".into()),
    )
    .unwrap();

    // The earlier report waits for the later one, which may overtake it.
    manager.recover_lost_reports().await;
    assert_eq!(state(&manager, &workspace).await, None);
    drop(sending);
    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_lost_report_whose_record_failed_is_recovered_on_the_next_pass() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    lose(&manager, &workspace, ended_turn(), "timed out");
    let fail_writes = |sql: &'static str| {
        let manager = manager.clone();
        async move {
            manager
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

    manager.recover_lost_reports().await;
    fail_writes("DROP TRIGGER full").await;
    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_recovered_report_outlasts_a_restart() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    // An earlier turn's end whose hook still waits on the daemon.
    let earlier = Sending::keep(
        &manager.paths,
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
    lose(&manager, &workspace, working, "timed out");
    manager.recover_lost_reports().await;
    drop(earlier);

    let paths = manager.paths.clone();
    drop(manager);
    let manager = Manager::open(paths).await.unwrap();
    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Working
    );
}

#[tokio::test]
async fn a_lost_report_whose_mark_fails_is_recorded_on_the_next_pass() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    lose(&manager, &workspace, ended_turn(), "timed out");
    let mark = manager
        .paths
        .hook_reports()
        .join(&workspace.id)
        .join(".delivered");
    std::fs::create_dir(&mark).unwrap();

    manager.recover_lost_reports().await;
    std::fs::remove_dir(&mark).unwrap();
    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Idle
    );
    assert!(hook_reports::last_delivered(&manager.paths, &workspace.id).is_some());
}

#[tokio::test]
async fn a_chats_lost_report_counts_only_from_the_agent_it_runs() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // The agent is a process this test's hooks run under.
    let chat = manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "claude".into(),
            std::os::unix::process::parent_id(),
            None,
            false,
        )
        .await
        .unwrap();
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    let current = |manager: Arc<Manager>, id: SpaceId| async move {
        manager
            .store
            .run(move |db| crate::daemon::workspace::agent_state::current(db, &id))
            .await
            .unwrap()
            .map(|status| status.state)
    };
    Sending::keep(&manager.paths, &chat.id, working, None)
        .unwrap()
        .failed("timed out".into());
    manager.recover_lost_reports().await;
    assert_eq!(
        current(manager.clone(), chat.id.clone()).await,
        Some(AgentState::Working)
    );

    Sending::keep(&manager.paths, &chat.id, ended_turn(), None)
        .unwrap()
        .failed("refused".into());
    // A restore runs another agent.
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE chats SET pid=1 WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.recover_lost_reports().await;

    assert_eq!(
        current(manager.clone(), chat.id.clone()).await,
        Some(AgentState::Working)
    );
    assert_eq!(failures(&manager).await.len(), 2);
}

#[tokio::test]
async fn a_report_whose_hook_died_after_its_answer_still_orders_a_restart() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    // An earlier turn's end whose hook still waits on the daemon.
    let earlier = Sending::keep(
        &manager.paths,
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
    let answered =
        Sending::keep(&manager.paths, &workspace.id, working, Some("agent".into())).unwrap();
    report(&manager, &workspace, AgentState::Working).await;
    drop(answered);
    manager.recover_lost_reports().await;
    drop(earlier);

    let paths = manager.paths.clone();
    drop(manager);
    let manager = Manager::open(paths).await.unwrap();
    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Working
    );
}

#[tokio::test]
async fn reports_are_ordered_by_when_their_hooks_made_them() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    let made = SystemTime::now();
    // The turn's end, made after the report that started its work, which
    // a stalled daemon recorded only afterwards.
    lose(&manager, &workspace, ended_turn(), "timed out");
    manager
        .report_agent_state(
            &SpaceSelector::id(&workspace.id),
            AgentState::Working,
            Some("agent".into()),
            None,
            None,
            Some(made),
        )
        .await
        .unwrap();

    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_live_report_made_before_a_recorded_one_changes_nothing() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    let made = SystemTime::now();
    lose(&manager, &workspace, ended_turn(), "timed out");
    manager.recover_lost_reports().await;

    // A report made before the turn's end, which a stalled daemon answers
    // only now.
    let status = manager
        .report_agent_state(
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
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Idle
    );
}

#[tokio::test]
async fn a_failed_turn_lost_before_a_later_idle_report_is_still_retried() {
    let (temp, manager) = manager().await;
    manager.publish_config(crate::daemon::test_support::global_config(
        "[agents.claude]\nretry = true\n",
    ));
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "reporter".into(), None, None, None)
        .await
        .unwrap();
    let started = manager
        .begin_execution(
            workspace.id.as_str(),
            None,
            crate::daemon::workspace::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .track_agent(
            &started.plan.id,
            &crate::execution::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            std::time::Duration::ZERO,
        )
        .await;
    let execution = Some(started.plan.id.clone());
    let lose_from = |report: Report| {
        Sending::keep(&manager.paths, &workspace.id, report, execution.clone())
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

    manager.recover_lost_reports().await;

    let id = workspace.id.clone();
    let retry = manager
        .store
        .run(move |db| crate::daemon::workspace::turn_retry::get(db, &id))
        .await
        .unwrap();
    assert!(retry.is_some(), "the failed turn is retried");
    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Idle
    );
    drop(started.stop);
}

#[tokio::test]
async fn a_lost_report_waits_for_a_later_one_its_hook_still_sends() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    lose(&manager, &workspace, ended_turn(), "timed out");
    // A later report the daemon recorded, whose hook has yet to mark it
    // delivered when the daemon restarts.
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    let answered =
        Sending::keep(&manager.paths, &workspace.id, working, Some("agent".into())).unwrap();
    let paths = manager.paths.clone();
    drop(manager);
    let manager = Manager::open(paths).await.unwrap();

    manager.recover_lost_reports().await;
    assert!(failures(&manager).await.is_empty());
    answered.delivered();
    manager.recover_lost_reports().await;

    assert_eq!(
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Working
    );
    assert_eq!(failures(&manager).await.len(), 1);
}

#[tokio::test]
async fn a_delayed_live_report_waits_behind_a_later_one_still_sent() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    report(&manager, &workspace, AgentState::Working).await;
    let made = SystemTime::now();
    // A later report the daemon recorded, whose hook has yet to mark it
    // delivered when the daemon restarts.
    let working = Report::State {
        state: AgentState::Working,
        session: None,
        turn_end: None,
    };
    let answered =
        Sending::keep(&manager.paths, &workspace.id, working, Some("agent".into())).unwrap();
    let paths = manager.paths.clone();
    drop(manager);
    let manager = Manager::open(paths).await.unwrap();

    // The earlier turn's end, which reaches the new daemon only now.
    manager
        .report_agent_state(
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
        state(&manager, &workspace).await.unwrap().state,
        AgentState::Working
    );
}
