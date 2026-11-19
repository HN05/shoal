use std::time::Duration;

use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

use super::record::{self, Recorded};
use crate::{
    config::permissions::PermissionChange,
    daemon::{
        executions::ExecutionKind,
        scope::Caller,
        test_support::{manager, repository},
        workspace::Manager,
    },
    model::{AgentRestart, RestartOutcome, RestartWait, Workspace},
    state::AgentState,
};

async fn workspace(root: &std::path::Path, manager: &Manager) -> Workspace {
    let repo = repository(root, "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "agents".into(), None, None, None)
        .await
        .unwrap()
}

async fn track(
    manager: &Manager,
    workspace: &Workspace,
    id: &ExecutionId,
    name: &str,
    recover: bool,
) {
    manager
        .track_agent(
            id,
            &crate::execution::TrackedAgent::named(name),
            &workspace.id,
            &workspace.name,
            recover,
            Duration::ZERO,
        )
        .await;
}

/// The reason the daemon gave for skipping the agent of `workspace`.
async fn skipped(manager: &Manager, workspace: &Workspace) -> String {
    let space = SpaceSelector::id(&workspace.id);
    let outcome = manager.restart_space(None, &space, AgentRestart::default());
    match outcome.await.unwrap() {
        RestartOutcome::Skipped { reason, .. } => reason,
        other => panic!("expected the restart skipped: {other:?}"),
    }
}

#[tokio::test]
async fn a_space_whose_agent_cannot_restart_is_skipped_with_why() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    assert!(skipped(&manager, &workspace).await.contains("no agent"));

    let custom = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(&manager, &workspace, &custom.plan.id, "custom", false).await;
    assert!(skipped(&manager, &workspace).await.contains("resume"));
    assert!(!*custom.stop.borrow());

    track(&manager, &workspace, &custom.plan.id, "claude", true).await;
    manager
        .stop_agent_for_overload("critical memory pressure")
        .await;
    let reason = skipped(&manager, &workspace).await;
    assert!(reason.contains("critical memory pressure"), "{reason}");
    assert!(
        reason.contains(&format!("shoal queue start {}", workspace.number)),
        "{reason}"
    );
}

#[tokio::test]
async fn a_space_named_restart_answers_once_its_agent_runs_again() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restart = AgentRestart {
        in_place: true,
        ..Default::default()
    };
    let restarting = tokio::spawn({
        let (manager, space) = (manager.clone(), SpaceSelector::id(&workspace.id));
        async move { manager.restart_space(None, &space, restart).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    let RestartOutcome::Restarted { agent, .. } = restarting.await.unwrap().unwrap() else {
        panic!("expected the agent restarted");
    };
    assert_eq!(agent.name(), "claude");
}

#[tokio::test]
async fn a_restart_waiting_for_processes_answers_what_it_waits_for() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarting = tokio::spawn({
        let (manager, space) = (manager.clone(), SpaceSelector::id(&workspace.id));
        async move {
            manager
                .restart_space(None, &space, AgentRestart::default())
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.notify_restore_waits(&id, "PID 7 survives").await;
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
async fn working(
    root: &std::path::Path,
    manager: &Manager,
) -> (Workspace, crate::daemon::executions::StartedExecution) {
    let workspace = workspace(root, manager).await;
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(manager, &workspace, &execution.plan.id, "claude", true).await;
    let space = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&space, AgentState::Working, None, None))
        .await
        .unwrap();
    (workspace, execution)
}

/// The restarts the daemon recorded.
async fn recorded(manager: &Manager) -> Vec<Recorded> {
    manager.store.read(|db| record::all(db)).await.unwrap()
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
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let outcome = manager.restart_space(None, &space, waiting()).await;
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
    };
    assert_eq!(recorded(&manager).await, [restart]);
    // The space shows how its agent restarts.
    let listed = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(listed.restart, Some(waiting()));
    // Nothing restarts while the turn goes on.
    manager.restart_finished_turns().await.unwrap();
    assert!(!*execution.stop.borrow());

    (manager.set_agent_state(&space, AgentState::Idle, None, None))
        .await
        .unwrap();
    manager.restart_finished_turns().await.unwrap();
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(manager.settle_restart(&execution.plan.id).await);
    // It stays recorded until it ends, for a daemon that takes over
    // meanwhile, and the watcher does not act on it again.
    assert_eq!(recorded(&manager).await.len(), 1);
    manager.restart_finished_turns().await.unwrap();
    run_again(&manager, &execution.plan.id).await;
    while !recorded(&manager).await.is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(!manager.restarting(&execution.plan.id).await);
}

#[tokio::test]
async fn a_restart_the_line_took_waits_for_the_turn_a_newer_request_waits_for() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let sequential = AgentRestart {
        sequential: true,
        ..Default::default()
    };
    // The agent idles, so the sequential restart joins the line at once.
    (manager.set_agent_state(&space, AgentState::Idle, None, None))
        .await
        .unwrap();
    let in_line = tokio::spawn({
        let (manager, space) = (manager.clone(), space.clone());
        async move { manager.restart_space(None, &space, sequential).await }
    });
    while recorded(&manager).await.is_empty() {
        tokio::task::yield_now().await;
    }
    (manager.set_agent_state(&space, AgentState::Working, None, None))
        .await
        .unwrap();
    (manager.restart_space(None, &space, waiting()))
        .await
        .unwrap();
    // The line takes the space it held, but the turn goes on.
    assert!(!manager.act_on_restart(&workspace.id).await);
    assert!(!*execution.stop.borrow());
    assert!(recorded(&manager).await[0].restart.wait);
    let waits = in_line.await.unwrap().unwrap();
    assert!(
        matches!(
            waits,
            RestartOutcome::Waits {
                waits: RestartWait::Turn,
                ..
            }
        ),
        "{waits:?}"
    );
}

#[tokio::test]
async fn a_recorded_restart_goes_when_its_agent_exits_first() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    (manager.restart_space(None, &space, waiting()))
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    (manager.finish_execution(id, ExecutionKind::Command, Some(0)))
        .await
        .unwrap();
    assert!(!manager.act_on_restart(&workspace.id).await);
    assert!(recorded(&manager).await.is_empty());
}

/// Record a restart of the agent of `workspace`, running as `execution`,
/// for `caller` once the turn ends, then fail it as the agent's wrapper
/// would, with no request left to answer.
async fn fail_after_the_turn(
    manager: &std::sync::Arc<Manager>,
    workspace: &Workspace,
    execution: &crate::daemon::executions::StartedExecution,
    caller: Option<&Caller>,
) {
    let space = SpaceSelector::id(&workspace.id);
    (manager.restart_space(caller, &space, waiting()))
        .await
        .unwrap();
    (manager.set_agent_state(&space, AgentState::Idle, None, None))
        .await
        .unwrap();
    let acting = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move { manager.act_on_restart(&id).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = &execution.plan.id;
    manager.fail_restart(id, FAILURE.into()).await;
    assert!(!acting.await.unwrap());
}

const FAILURE: &str = "automatic restore failed";

#[tokio::test]
async fn a_deferred_restart_that_fails_reaches_the_user() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    fail_after_the_turn(&manager, &workspace, &execution, None).await;
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert!(
        notifications.iter().any(|notice| notice.kind
            == crate::daemon::notifications::NotificationKind::RecoveryBlocked
            && notice.message.contains(FAILURE)),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn a_deferred_restart_that_fails_reaches_the_coordinator_that_asked() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let repository = workspace.repository().clone();
    let coordinator = manager
        .create_workspace(&repository, "coordinator".into(), None, None, None)
        .await
        .unwrap();
    let caller = Caller {
        space_id: coordinator.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    };
    fail_after_the_turn(&manager, &workspace, &execution, Some(&caller)).await;
    let coordinator = SpaceSelector::id(&coordinator.id);
    let messages = manager.agent_messages(&coordinator).await.unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.message.contains(FAILURE)),
        "{messages:?}"
    );
}

#[tokio::test]
async fn a_sequential_restart_answers_once_the_line_restarted_its_agent() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let line = tokio::spawn(super::run_line(manager.clone()));
    let restart = AgentRestart {
        sequential: true,
        in_place: true,
        ..Default::default()
    };
    let restarting = tokio::spawn({
        let (manager, space) = (manager.clone(), SpaceSelector::id(&workspace.id));
        async move { manager.restart_space(None, &space, restart).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    let outcome = restarting.await.unwrap().unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Restarted { .. }),
        "{outcome:?}"
    );
    assert!(recorded(&manager).await.is_empty());
    line.abort();
}

/// Start the restarted agent of `id` again, as its wrapper does once it
/// stopped.
async fn run_again(manager: &Manager, id: &ExecutionId) {
    manager.settle_restart(id).await;
    assert!(manager.resume_agent_execution(id, None).await.unwrap());
    manager.agent_started(id).await;
}

#[tokio::test]
async fn a_newer_request_restarts_now_with_the_permission_change_of_the_one_it_supersedes() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let push = PermissionChange::assigned("push=direct").unwrap();
    let waiting = AgentRestart {
        change: Some(push.clone()),
        ..waiting()
    };
    (manager.restart_space(None, &space, waiting))
        .await
        .unwrap();
    let restarting = tokio::spawn({
        let (manager, space) = (manager.clone(), space.clone());
        async move {
            manager
                .restart_space(None, &space, AgentRestart::default())
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();
    assert_eq!(manager.restart_change(&id).await, Some(push));
    assert!(recorded(&manager).await.is_empty());
    run_again(&manager, &id).await;
    let outcome = restarting.await.unwrap().unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Restarted { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_request_after_the_restart_settled_restarts_the_agent_again_once_it_runs() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let in_place = AgentRestart {
        in_place: true,
        ..Default::default()
    };
    let request = |restart: AgentRestart| {
        let (manager, space) = (manager.clone(), space.clone());
        tokio::spawn(async move { manager.restart_space(None, &space, restart).await })
    };
    let first = request(in_place.clone());
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();
    assert!(manager.settle_restart(&id).await);
    let second = request(in_place.clone());
    // The second waits for the agent the first starts.
    while recorded(&manager).await.is_empty() {
        tokio::task::yield_now().await;
    }
    run_again(&manager, &id).await;
    first.await.unwrap().unwrap();
    assert!(!manager.restarting(&id).await);

    manager.restart_finished_turns().await.unwrap();
    while !manager.restarting(&id).await {
        tokio::task::yield_now().await;
    }
    run_again(&manager, &id).await;
    let outcome = second.await.unwrap().unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Restarted { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_cancel_drops_the_recorded_restart_and_its_permission_change() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let restart = AgentRestart {
        change: Some(PermissionChange::assigned("push=direct").unwrap()),
        ..waiting()
    };
    (manager.restart_space(None, &space, restart.clone()))
        .await
        .unwrap();
    assert_eq!(manager.cancel_restart(&space).await.unwrap(), Some(restart));
    assert!(recorded(&manager).await.is_empty());
    assert_eq!(manager.cancel_restart(&space).await.unwrap(), None);

    // A later restart no longer takes the dropped change.
    let restarting = tokio::spawn({
        let (manager, space) = (manager.clone(), space.clone());
        async move {
            manager
                .restart_space(None, &space, AgentRestart::default())
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();
    assert_eq!(manager.restart_change(&id).await, None);
    run_again(&manager, &id).await;
    restarting.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_end_of_a_recorded_restart_keeps_an_identical_newer_one() {
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    (manager.restart_space(None, &space, waiting()))
        .await
        .unwrap();
    (manager.set_agent_state(&space, AgentState::Idle, None, None))
        .await
        .unwrap();
    manager.restart_finished_turns().await.unwrap();
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();
    assert!(manager.settle_restart(&id).await);
    // The same request again, once the first settled, restarts the agent
    // again after it runs.
    (manager.restart_space(None, &space, waiting()))
        .await
        .unwrap();
    run_again(&manager, &id).await;
    while manager.restarts.acting(&workspace.id) {
        tokio::task::yield_now().await;
    }
    assert_eq!(recorded(&manager).await.len(), 1);
}

#[tokio::test]
async fn a_request_whose_restart_went_with_its_space_is_answered() {
    let (root, manager) = manager().await;
    let (workspace, _execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&space, AgentState::Idle, None, None))
        .await
        .unwrap();
    let sequential = AgentRestart {
        sequential: true,
        ..Default::default()
    };
    let in_line = tokio::spawn({
        let (manager, space) = (manager.clone(), space.clone());
        async move { manager.restart_space(None, &space, sequential).await }
    });
    while recorded(&manager).await.is_empty() {
        tokio::task::yield_now().await;
    }
    // As the removal of the space deletes its record.
    (manager.store)
        .run(|db| Ok(db.execute("DELETE FROM agent_restarts", [])?))
        .await
        .unwrap();
    manager.restart_finished_turns().await.unwrap();
    let outcome = in_line.await.unwrap().unwrap();
    assert!(
        matches!(outcome, RestartOutcome::Skipped { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_recorded_restart_goes_with_its_agent_rather_than_restart_the_next() {
    let (root, manager) = manager().await;
    let (workspace, first) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    (manager.restart_space(None, &space, waiting()))
        .await
        .unwrap();
    let ended = first.plan.id.clone();
    (manager.finish_execution(ended, ExecutionKind::Command, Some(0)))
        .await
        .unwrap();
    let next = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(&manager, &workspace, &next.plan.id, "claude", true).await;
    (manager.set_agent_state(&space, AgentState::Idle, None, None))
        .await
        .unwrap();
    assert!(!manager.act_on_restart(&workspace.id).await);
    assert!(!*next.stop.borrow());
    assert!(recorded(&manager).await.is_empty());
}

#[tokio::test]
async fn a_wrapper_that_reattaches_continues_the_begun_restart_with_its_options() {
    use crate::daemon::workspace::agents::ProtectionExt as _;
    let (root, manager) = manager().await;
    let (workspace, execution) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let change = PermissionChange::assigned("push=direct").unwrap();
    let restart = AgentRestart {
        change: Some(change.clone()),
        ..waiting()
    };
    (manager.restart_space(None, &space, restart))
        .await
        .unwrap();
    (manager.set_agent_state(&space, AgentState::Idle, None, None))
        .await
        .unwrap();
    manager.restart_finished_turns().await.unwrap();
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    let id = execution.plan.id.clone();

    // As the wrapper reattaches to a daemon that knows the restart only
    // from its record.
    let protection = crate::model::Protection::restart();
    manager.await_agent_restore(&id, protection).await;
    assert_eq!(manager.restart_change(&id).await, Some(change));
    assert!(manager.settle_restart(&id).await);
    assert!(recorded(&manager).await.is_empty());
}

#[tokio::test]
async fn a_recorded_restart_follows_its_agent_to_a_new_execution() {
    let (_root, manager) = manager().await;
    let recorded_for = |execution: &str| Recorded {
        space_id: "space".into(),
        execution: execution.into(),
        restart: waiting(),
        asked_by: None,
    };
    let old = recorded_for("old");
    manager
        .store
        .write(move |tx| {
            record::put(tx, &old)?;
            record::follow(tx, &"old".into(), &"new".into())
        })
        .await
        .unwrap();
    assert_eq!(recorded(&manager).await, [recorded_for("new")]);
}

#[tokio::test]
async fn a_newer_request_for_another_agent_takes_no_permission_change_of_the_one_before() {
    let (root, manager) = manager().await;
    let (workspace, first) = working(root.path(), &manager).await;
    let space = SpaceSelector::id(&workspace.id);
    let restart = AgentRestart {
        change: Some(PermissionChange::assigned("push=direct").unwrap()),
        ..waiting()
    };
    (manager.restart_space(None, &space, restart))
        .await
        .unwrap();
    let ended = first.plan.id.clone();
    (manager.finish_execution(ended, ExecutionKind::Command, Some(0)))
        .await
        .unwrap();
    let next = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(&manager, &workspace, &next.plan.id, "claude", true).await;
    (manager.restart_space(None, &space, waiting()))
        .await
        .unwrap();
    let [recorded] = recorded(&manager).await.try_into().unwrap();
    assert_eq!(recorded.execution, next.plan.id);
    assert_eq!(recorded.restart.change, None);
}
