use std::{sync::Arc, time::Duration};

use shoal_daemon_base::callers::Caller;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

use super::{
    deferred::Standing,
    record::{self, Recorded},
};
use crate::{
    config::permissions::PermissionChange,
    executions::ExecutionKind,
    model::{AgentRestart, RestartOutcome, RestartWait, Workspace},
    stand_in::Fixture,
    state::AgentState,
};

async fn track(
    fixture: &Fixture,
    workspace: &Workspace,
    id: &ExecutionId,
    name: &str,
    recover: bool,
) {
    fixture
        .agents
        .track_agent(
            &*fixture.daemon,
            id,
            &shoal_protocol::executions::TrackedAgent::named(name),
            &workspace.id,
            &workspace.name,
            recover,
            Duration::ZERO,
        )
        .await;
}

/// The reason the daemon gave for skipping the agent of `workspace`.
async fn skipped(fixture: &Fixture, workspace: &Workspace) -> String {
    let space = SpaceSelector::id(&workspace.id);
    let outcome =
        fixture
            .agents
            .restart_space(&*fixture.daemon, None, &space, AgentRestart::default());
    match outcome.await.unwrap() {
        RestartOutcome::Skipped { reason, .. } => reason,
        other => panic!("expected the restart skipped: {other:?}"),
    }
}

#[tokio::test]
async fn a_space_whose_agent_cannot_restart_is_skipped_with_why() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = fixture.workspace("agents").await;
    assert!(skipped(&fixture, &workspace).await.contains("no agent"));

    let custom = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    track(&fixture, &workspace, &custom.plan.id, "custom", false).await;
    assert!(skipped(&fixture, &workspace).await.contains("resume"));
    assert!(!*custom.stop.borrow());

    track(&fixture, &workspace, &custom.plan.id, "claude", true).await;
    fixture
        .agents
        .stop_for_overload(&*fixture.daemon, crate::overload::Pressure::Memory)
        .await;
    let reason = skipped(&fixture, &workspace).await;
    assert!(reason.contains("critical memory pressure"), "{reason}");
    assert!(
        reason.contains(&format!("shoal queue start {}", workspace.number)),
        "{reason}"
    );
}

#[tokio::test]
async fn a_space_named_restart_answers_once_its_agent_runs_again() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = fixture.workspace("agents").await;
    let execution = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&fixture, &workspace, &id, "claude", true).await;
    let restart = AgentRestart {
        in_place: true,
        ..Default::default()
    };
    let restarting = tokio::spawn({
        let (fixture, space) = (fixture.clone(), SpaceSelector::id(&workspace.id));
        async move {
            fixture
                .agents
                .restart_space(&*fixture.daemon, None, &space, restart)
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, &id, None)
            .await
            .unwrap()
    );
    fixture.agents.agent_started(&id).await;
    let RestartOutcome::Restarted { agent, .. } = restarting.await.unwrap().unwrap() else {
        panic!("expected the agent restarted");
    };
    assert_eq!(agent.name(), "claude");
    assert!(recorded(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_restart_waiting_for_processes_answers_what_it_waits_for() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = fixture.workspace("agents").await;
    let execution = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&fixture, &workspace, &id, "claude", true).await;
    let restarting = tokio::spawn({
        let (fixture, space) = (fixture.clone(), SpaceSelector::id(&workspace.id));
        async move {
            fixture
                .agents
                .restart_space(&*fixture.daemon, None, &space, AgentRestart::default())
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    fixture
        .agents
        .notify_restore_waits(&*fixture.daemon, &id, "PID 7 survives")
        .await;
    let waits = RestartWait::Processes {
        issue: "PID 7 survives".into(),
    };
    assert_eq!(
        restarting.await.unwrap().unwrap(),
        RestartOutcome::Waits {
            agent: "claude".into(),
            waits,
        }
    );
}

/// A workspace whose `claude` agent runs as the returned execution and
/// works on a turn.
async fn working(fixture: &Fixture) -> (Workspace, crate::executions::StartedExecution) {
    let workspace = fixture.workspace("agents").await;
    let execution = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    track(fixture, &workspace, &execution.plan.id, "claude", true).await;
    let space = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .set_agent_state(&*fixture.daemon, &space, AgentState::Working, None, None))
    .await
    .unwrap();
    (workspace, execution)
}

/// The restarts the daemon recorded.
async fn recorded(fixture: &Fixture) -> Vec<Recorded> {
    fixture
        .agents
        .store
        .read(|db| record::all(db))
        .await
        .unwrap()
}

fn waiting() -> AgentRestart {
    AgentRestart {
        wait: true,
        in_place: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_restart_waiting_for_the_turn_is_recorded_until_the_turn_ends() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    let outcome = fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, waiting())
        .await;
    let waits = RestartOutcome::Waits {
        agent: "claude".into(),
        waits: RestartWait::Turn,
    };
    assert_eq!(outcome.unwrap(), waits);
    let restart = Recorded {
        space_id: workspace.id.clone(),
        execution: execution.plan.id.clone(),
        restart: waiting(),
        asked_by: None,
        begun: false,
    };
    assert_eq!(recorded(&fixture).await, [restart]);
    // The space shows how its agent restarts.
    let id = workspace.id.clone();
    let listed = (fixture.agents.store)
        .run(move |db| record::current(db, &id))
        .await
        .unwrap();
    assert_eq!(listed, Some(waiting()));
    // Nothing restarts while the turn goes on.
    fixture
        .agents
        .restart_finished_turns(&(fixture.daemon.clone() as Arc<dyn crate::daemon::Daemon>))
        .await
        .unwrap();
    assert!(!*execution.stop.borrow());

    (fixture
        .agents
        .set_agent_state(&*fixture.daemon, &space, AgentState::Idle, None, None))
    .await
    .unwrap();
    fixture
        .agents
        .restart_finished_turns(&(fixture.daemon.clone() as Arc<dyn crate::daemon::Daemon>))
        .await
        .unwrap();
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(fixture.agents.settle_restart(&execution.plan.id).await);
    // It stays recorded until it ends, for a daemon that takes over
    // meanwhile, and the watcher does not act on it again.
    assert_eq!(recorded(&fixture).await.len(), 1);
    fixture
        .agents
        .restart_finished_turns(&(fixture.daemon.clone() as Arc<dyn crate::daemon::Daemon>))
        .await
        .unwrap();
    run_again(&fixture, &execution.plan.id).await;
    while !recorded(&fixture).await.is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(!fixture.agents.restarting(&execution.plan.id).await);
}

#[tokio::test]
async fn a_restart_the_line_took_waits_for_the_turn_a_newer_request_waits_for() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    let sequential = AgentRestart {
        sequential: true,
        ..Default::default()
    };
    // The agent idles, so the sequential restart joins the line at once.
    (fixture
        .agents
        .set_agent_state(&*fixture.daemon, &space, AgentState::Idle, None, None))
    .await
    .unwrap();
    let in_line = (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, sequential))
    .await
    .unwrap();
    // It answers once in the line, which keeps it.
    assert!(
        matches!(
            in_line,
            RestartOutcome::Waits {
                waits: RestartWait::Line,
                ..
            }
        ),
        "{in_line:?}"
    );
    (fixture
        .agents
        .set_agent_state(&*fixture.daemon, &space, AgentState::Working, None, None))
    .await
    .unwrap();
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, waiting()))
    .await
    .unwrap();
    // The line takes the space it held, but the turn goes on.
    assert!(
        !fixture
            .agents
            .act_on_restart(&*fixture.daemon, &workspace.id)
            .await
    );
    assert!(!*execution.stop.borrow());
    assert!(recorded(&fixture).await[0].restart.wait);
}

#[tokio::test]
async fn a_recorded_restart_goes_when_its_agent_exits_first() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, waiting()))
    .await
    .unwrap();
    let id = execution.plan.id.clone();
    (fixture
        .agents
        .finish_execution(&*fixture.daemon, id, ExecutionKind::Command, Some(0)))
    .await
    .unwrap();
    assert!(
        !fixture
            .agents
            .act_on_restart(&*fixture.daemon, &workspace.id)
            .await
    );
    assert!(recorded(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_recorded_restart_waits_for_the_agent_of_a_reattaching_wrapper() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, waiting()))
    .await
    .unwrap();
    // A daemon that took over connects the wrapper before it tracks the
    // agent again.
    let id = &execution.plan.id;
    fixture.agents.detach_execution(id).await;
    let token = execution.plan.scope_token.clone();
    let kind = ExecutionKind::Command;
    let mut connections = fixture.agents.executions().connections().await;
    let _stop = (connections.connect(id, kind, &workspace.id, token, None, false)).await;
    drop(connections);
    let [restart] = recorded(&fixture).await.try_into().unwrap();
    let agents = fixture.agents.running_agents().await.unwrap();
    let standing = fixture.agents.standing(&restart, &agents).await.unwrap();
    assert!(matches!(standing, Standing::Waits));
}

/// Record a restart of the agent of `workspace`, running as `execution`,
/// for `caller` once the turn ends, then fail it as the agent's wrapper
/// would, with no request left to answer.
async fn fail_after_the_turn(
    fixture: &Arc<Fixture>,
    workspace: &Workspace,
    execution: &crate::executions::StartedExecution,
    caller: Option<&Caller>,
) {
    let space = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .restart_space(&*fixture.daemon, caller, &space, waiting()))
    .await
    .unwrap();
    (fixture
        .agents
        .set_agent_state(&*fixture.daemon, &space, AgentState::Idle, None, None))
    .await
    .unwrap();
    let acting = tokio::spawn({
        let (fixture, id) = (fixture.clone(), workspace.id.clone());
        async move { fixture.agents.act_on_restart(&*fixture.daemon, &id).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = &execution.plan.id;
    fixture.agents.fail_restart(id, FAILURE.into()).await;
    assert!(!acting.await.unwrap());
}

const FAILURE: &str = "automatic restore failed";

#[tokio::test]
async fn a_deferred_restart_that_fails_reaches_the_user() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    fail_after_the_turn(&fixture, &workspace, &execution, None).await;
    let notified = fixture.daemon.notified.lock().unwrap();
    assert!(
        (notified.iter()).any(|(kind, message)| *kind
            == shoal_daemon_base::alerts::AlertKind::RecoveryBlocked
            && message.contains(FAILURE)),
        "{notified:?}"
    );
}

#[tokio::test]
async fn a_sequential_restart_answers_once_in_the_line_which_restarts_its_agent() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = fixture.workspace("agents").await;
    let execution = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&fixture, &workspace, &id, "claude", true).await;
    let line = tokio::spawn(crate::restarts::run_line(
        fixture.agents.clone(),
        fixture.daemon.clone(),
    ));
    let restart = AgentRestart {
        sequential: true,
        in_place: true,
        ..Default::default()
    };
    let space = SpaceSelector::id(&workspace.id);
    let outcome = fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, restart)
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            RestartOutcome::Waits {
                waits: RestartWait::Line,
                ..
            }
        ),
        "{outcome:?}"
    );
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, &id, None)
            .await
            .unwrap()
    );
    fixture.agents.agent_started(&id).await;
    while !recorded(&fixture).await.is_empty() {
        tokio::task::yield_now().await;
    }
    line.abort();
}

/// Start the fixture agent of `id` again, as its wrapper does once it
/// stopped.
async fn run_again(fixture: &Fixture, id: &ExecutionId) {
    fixture.agents.settle_restart(id).await;
    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, id, None)
            .await
            .unwrap()
    );
    fixture.agents.agent_started(id).await;
}

#[tokio::test]
async fn a_newer_request_restarts_now_with_the_permission_change_of_the_one_it_supersedes() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    let push = PermissionChange::assigned("push=direct").unwrap();
    let waiting = AgentRestart {
        change: Some(push.clone()),
        ..waiting()
    };
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, waiting))
    .await
    .unwrap();
    let restarting = tokio::spawn({
        let (fixture, space) = (fixture.clone(), space.clone());
        async move {
            fixture
                .agents
                .restart_space(&*fixture.daemon, None, &space, AgentRestart::default())
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();
    assert_eq!(fixture.agents.restart_change(&id).await, Some(push.clone()));
    let [begun] = &recorded(&fixture).await[..] else {
        panic!("expected the restart recorded");
    };
    assert!(begun.begun);
    assert_eq!(begun.restart.change, Some(push));
    run_again(&fixture, &id).await;
    let outcome = restarting.await.unwrap().unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Restarted { .. }),
        "{outcome:?}"
    );
    assert!(recorded(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_request_after_the_restart_settled_restarts_the_agent_again_once_it_runs() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    let in_place = AgentRestart {
        in_place: true,
        ..Default::default()
    };
    let request = |restart: AgentRestart| {
        let (fixture, space) = (fixture.clone(), space.clone());
        tokio::spawn(async move {
            fixture
                .agents
                .restart_space(&*fixture.daemon, None, &space, restart)
                .await
        })
    };
    let first = request(in_place.clone());
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();
    assert!(fixture.agents.settle_restart(&id).await);
    let second = request(in_place.clone());
    // The second waits for the agent the first starts, recorded in place
    // of the first's begun record.
    while !recorded(&fixture).await.iter().any(|kept| !kept.begun) {
        tokio::task::yield_now().await;
    }
    run_again(&fixture, &id).await;
    first.await.unwrap().unwrap();
    assert!(!fixture.agents.restarting(&id).await);

    fixture
        .agents
        .restart_finished_turns(&(fixture.daemon.clone() as Arc<dyn crate::daemon::Daemon>))
        .await
        .unwrap();
    while !fixture.agents.restarting(&id).await {
        tokio::task::yield_now().await;
    }
    run_again(&fixture, &id).await;
    let outcome = second.await.unwrap().unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Restarted { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_cancel_drops_the_recorded_restart_and_its_permission_change() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    let restart = AgentRestart {
        change: Some(PermissionChange::assigned("push=direct").unwrap()),
        ..waiting()
    };
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, restart.clone()))
    .await
    .unwrap();
    assert_eq!(
        fixture
            .agents
            .cancel_restart(&*fixture.daemon, &space)
            .await
            .unwrap(),
        Some(restart)
    );
    assert!(recorded(&fixture).await.is_empty());
    assert_eq!(
        fixture
            .agents
            .cancel_restart(&*fixture.daemon, &space)
            .await
            .unwrap(),
        None
    );

    // A later restart no longer takes the dropped change.
    let restarting = tokio::spawn({
        let (fixture, space) = (fixture.clone(), space.clone());
        async move {
            fixture
                .agents
                .restart_space(&*fixture.daemon, None, &space, AgentRestart::default())
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();
    assert_eq!(fixture.agents.restart_change(&id).await, None);
    run_again(&fixture, &id).await;
    restarting.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_end_of_a_recorded_restart_keeps_an_identical_newer_one() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, waiting()))
    .await
    .unwrap();
    (fixture
        .agents
        .set_agent_state(&*fixture.daemon, &space, AgentState::Idle, None, None))
    .await
    .unwrap();
    fixture
        .agents
        .restart_finished_turns(&(fixture.daemon.clone() as Arc<dyn crate::daemon::Daemon>))
        .await
        .unwrap();
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();
    assert!(fixture.agents.settle_restart(&id).await);
    // The same request again, once the first settled, restarts the agent
    // again after it runs.
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, waiting()))
    .await
    .unwrap();
    run_again(&fixture, &id).await;
    while fixture.agents.restarts().acting(&workspace.id) {
        tokio::task::yield_now().await;
    }
    assert_eq!(recorded(&fixture).await.len(), 1);
}

#[tokio::test]
async fn a_request_whose_restart_went_with_its_space_is_answered() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, execution) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    let in_place = AgentRestart {
        in_place: true,
        ..Default::default()
    };
    let request = || {
        let (fixture, space, restart) = (fixture.clone(), space.clone(), in_place.clone());
        tokio::spawn(async move {
            fixture
                .agents
                .restart_space(&*fixture.daemon, None, &space, restart)
                .await
        })
    };
    let _first = request();
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(fixture.agents.settle_restart(&execution.plan.id).await);
    // It waits for the agent the first starts.
    let second = request();
    while !recorded(&fixture).await.iter().any(|kept| !kept.begun) {
        tokio::task::yield_now().await;
    }
    // As the removal of the space deletes its record.
    (fixture.agents.store)
        .run(|db| Ok(db.execute("DELETE FROM agent_restarts", [])?))
        .await
        .unwrap();
    fixture
        .agents
        .restart_finished_turns(&(fixture.daemon.clone() as Arc<dyn crate::daemon::Daemon>))
        .await
        .unwrap();
    let outcome = second.await.unwrap().unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Skipped { .. }),
        "{outcome:?}"
    );
}

fn recorded_for(execution: &str) -> Recorded {
    Recorded {
        space_id: "space".into(),
        execution: execution.into(),
        restart: waiting(),
        asked_by: None,
        begun: false,
    }
}

#[tokio::test]
async fn a_recorded_restart_follows_its_agent_to_a_new_execution() {
    let fixture = Arc::new(Fixture::new().await);
    let old = recorded_for("old");
    fixture
        .agents
        .store
        .write(move |tx| {
            record::put(tx, &old)?;
            record::follow(tx, &"old".into(), &"new".into())
        })
        .await
        .unwrap();
    assert_eq!(recorded(&fixture).await, [recorded_for("new")]);
}

#[tokio::test]
async fn the_new_wrapper_of_a_begun_restart_applies_it() {
    let fixture = Arc::new(Fixture::new().await);
    let old = recorded_for("old");
    fixture
        .agents
        .store
        .write(move |tx| {
            record::put(tx, &old)?;
            let (_, generation) = record::find_generation(tx, &old.space_id)?.unwrap();
            record::begin(tx, &old.space_id, &generation)?;
            record::follow(tx, &"old".into(), &"new".into())
        })
        .await
        .unwrap();
    assert!(recorded(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_newer_request_for_another_agent_takes_no_permission_change_of_the_one_before() {
    let fixture = Arc::new(Fixture::new().await);
    let (workspace, first) = working(&fixture).await;
    let space = SpaceSelector::id(&workspace.id);
    let restart = AgentRestart {
        change: Some(PermissionChange::assigned("push=direct").unwrap()),
        ..waiting()
    };
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, restart))
    .await
    .unwrap();
    let ended = first.plan.id.clone();
    (fixture
        .agents
        .finish_execution(&*fixture.daemon, ended, ExecutionKind::Command, Some(0)))
    .await
    .unwrap();
    let next = fixture
        .agents
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    track(&fixture, &workspace, &next.plan.id, "claude", true).await;
    (fixture
        .agents
        .restart_space(&*fixture.daemon, None, &space, waiting()))
    .await
    .unwrap();
    let [recorded] = recorded(&fixture).await.try_into().unwrap();
    assert_eq!(recorded.execution, next.plan.id);
    assert_eq!(recorded.restart.change, None);
}
