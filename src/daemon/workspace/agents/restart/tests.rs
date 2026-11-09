use crate::client::execution_ids::ExecutionId;
use crate::execution::recovery::RecordExt;
use std::time::Duration;

use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        test_support::{manager, repository},
        workspace::{ExecutionKind, Manager, RestartWaits},
    },
    execution::recovery,
    model::{PendingRestart, RunningAgent, Workspace},
    sessions::{HostName, Origin},
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

#[tokio::test]
async fn agents_list_connected_agents_and_executions_whose_wrapper_is_gone() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let command = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let agent = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let gone = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(&manager, &workspace, &agent.plan.id, "claude", true).await;
    manager.detach_execution(&gone.plan.id).await;
    // A report from the agent is its own state, unlike one from a command.
    manager
        .set_agent_state(
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            Some(agent.plan.id.clone()),
            None,
        )
        .await
        .unwrap();

    let mut agents = manager.running_agents().await.unwrap();
    agents.sort_by_key(|agent| agent.agent.is_none());
    assert_eq!(
        agents,
        [
            RunningAgent {
                execution: agent.plan.id.clone(),
                space_id: workspace.id.clone(),
                agent: Some("claude".into()),
                restorable: true,
                state: Some(AgentState::Idle),
                interrupted: false,
                origin: None,
                restarted_by: Vec::new(),
                restart: None,
                stopped: None,
                restarted_from: Vec::new(),
            },
            RunningAgent {
                execution: gone.plan.id.clone(),
                space_id: workspace.id.clone(),
                agent: None,
                restorable: false,
                state: None,
                interrupted: false,
                origin: None,
                restarted_by: Vec::new(),
                restart: None,
                stopped: None,
                restarted_from: Vec::new(),
            },
        ]
    );
    // A connected command is not an agent to restart.
    assert!(
        agents
            .iter()
            .all(|agent| agent.execution != command.plan.id)
    );
}

#[tokio::test]
async fn agents_are_listed_by_the_names_agent_selection_takes() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    // Happy records its agents as `happy codex`; `--agent` spells it `happy-codex`.
    track(
        &manager,
        &workspace,
        &execution.plan.id,
        "happy codex",
        true,
    )
    .await;
    let agents = manager.running_agents().await.unwrap();
    assert_eq!(agents[0].agent.as_deref(), Some("happy-codex"));
}

#[tokio::test]
async fn each_agent_keeps_its_own_turn_report_in_a_shared_workspace() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let mut ids = Vec::new();
    for name in ["claude", "codex"] {
        let execution = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        track(&manager, &workspace, &execution.plan.id, name, true).await;
        ids.push(execution.plan.id);
    }
    // The workspace keeps only the later report; each agent keeps its own.
    for (id, state) in ids.iter().zip([AgentState::Idle, AgentState::Working]) {
        manager
            .set_agent_state(
                &SpaceSelector::id(&workspace.id),
                state,
                Some(id.clone()),
                None,
            )
            .await
            .unwrap();
    }
    let states: std::collections::HashMap<_, _> = manager
        .running_agents()
        .await
        .unwrap()
        .into_iter()
        .map(|agent| (agent.execution, agent.state))
        .collect();
    assert_eq!(states[&ids[0]], Some(AgentState::Idle));
    assert_eq!(states[&ids[1]], Some(AgentState::Working));
    let status = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(
        status.workspace.agent_state.map(|status| status.state),
        Some(AgentState::Working)
    );
}

#[tokio::test]
async fn restart_refuses_agents_it_cannot_restore_without_stopping_them() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let custom = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let stopped = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    track(&manager, &workspace, &custom.plan.id, "custom", false).await;
    track(&manager, &workspace, &stopped.plan.id, "claude", true).await;
    manager
        .stop_agent_for_overload("critical memory pressure")
        .await;
    assert!(*stopped.stop.borrow());

    manager
        .restart_agent(&custom.plan.id, None, true, None, None)
        .await
        .unwrap_err();
    assert!(!*custom.stop.borrow());
    // A refused restart saves no handoff.
    assert!(!recovery::record_path(&manager.paths, &workspace.id, &custom.plan.id).exists());
    // The agent waits for its restore, which is no stop in progress.
    let refused = manager
        .restart_agent(&stopped.plan.id, None, true, None, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("critical memory pressure"), "{refused}");
    assert!(!refused.contains("already stopping"), "{refused}");
    assert!(!manager.restart_requested(&stopped.plan.id).await);
    manager
        .restart_agent(&ExecutionId::from("unknown"), None, true, None, None)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn a_restart_answers_once_the_agent_runs_again_or_with_why_not() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let place = Origin::created(HostName::Zmx, "agents".into());
    let execution = manager
        .begin_execution_placed(
            &SpaceSelector::id(&workspace.id),
            None,
            ExecutionKind::Command,
            None,
            crate::daemon::workspace::ExecutionRequest {
                place: Some(crate::daemon::workspace::places::ExecutionPlace {
                    origin: place.clone(),
                    agent: true,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;

    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id, None, true, None, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(manager.restart_requested(&id).await);
    // The handoff is saved before the stop reaches the wrapper.
    let handoff =
        recovery::Record::load(&recovery::record_path(&manager.paths, &workspace.id, &id))
            .unwrap()
            .unwrap();
    assert_eq!(handoff.stage, Some(recovery::Stage::Stopping));
    assert_eq!(
        handoff.stop_reason.as_deref(),
        Some("restarted by shoal restart")
    );
    // A first start that registers only now is not the restarted agent, so
    // the wrapper still receives the restart's stop.
    manager.agent_started(&id).await;
    assert!(manager.restart_requested(&id).await);
    // The restore resumes the agent without the queue, and its start answers.
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    assert!(!*stop.borrow_and_update());
    assert!(manager.restart_requested(&id).await);
    manager.agent_started(&id).await;
    // It answers with the place of the wrapper that runs the agent.
    assert_eq!(restarting.await.unwrap().unwrap(), Some(place));
    assert!(!manager.restart_requested(&id).await);
    assert!(manager.notifications(false, 10).await.unwrap().is_empty());

    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id, None, true, None, None).await }
    });
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.fail_restart(&id, "cancelled".into()).await;
    let error = restarting.await.unwrap().unwrap_err();
    assert_eq!(format!("{error}"), "cancelled");
}

#[tokio::test]
async fn a_restart_waiting_for_processes_answers_at_once_and_still_resumes() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id, None, true, None, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();

    let issue = "unreadable live process environments (PID 7)";
    manager.notify_restore_waits(&id, issue).await;
    let error = restarting.await.unwrap().unwrap_err();
    assert!(error.downcast_ref::<RestartWaits>().is_some());
    assert!(format!("{error}").contains(issue), "{error}");
    // The restart goes on, and its start no longer has a request to answer.
    assert!(manager.restart_requested(&id).await);
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    assert!(!manager.restart_requested(&id).await);
}

#[tokio::test]
async fn a_second_request_joins_the_restart_in_progress() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let first = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move {
            manager
                .restart_agent(&id, Some("first".into()), true, None, None)
                .await
        }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    // Registered before the agent starts again, which answers it; the
    // restart keeps the run that asked for it first.
    let second = manager
        .request_restart(&id, Some("second".into()), true, None, None)
        .await
        .unwrap();
    let listed = manager.running_agents().await.unwrap();
    assert_eq!(
        listed[0].restart,
        Some(PendingRestart {
            run: Some("first".into()),
            joined: vec!["second".into()],
        })
    );
    // The agent's start answers both.
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    let listed = manager.running_agents().await.unwrap();
    assert_eq!(listed[0].restart, None);
    // The run that joined got its restart too.
    assert_eq!(listed[0].restarted_by, ["first", "second"]);
    // Joining once the restart ended finds the agent running again, and
    // starts no other restart.
    manager
        .join_restart(&id, Some("first".into()), Some("first".into()))
        .await
        .unwrap();
    assert!(!manager.restart_requested(&id).await);
    assert!(!*stop.borrow());
}

#[tokio::test]
async fn a_request_joining_a_waiting_restart_is_answered_with_what_it_waits_for_now() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarting = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move { manager.restart_agent(&id, None, true, None, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.notify_restore_waits(&id, "PID 7 survives").await;
    restarting.await.unwrap().unwrap_err();
    // Other processes keep it waiting now.
    manager.restart_waits(&id, "PID 8 survives").await;

    let error = manager
        .restart_agent(&id, None, true, None, None)
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<RestartWaits>().is_some());
    assert!(format!("{error}").contains("PID 8 survives"), "{error}");
    assert!(manager.restart_requested(&id).await);
}

#[tokio::test]
async fn a_late_join_answers_as_the_restart_ended() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    manager.join_restart(&id, None, None).await.unwrap_err();

    let failed = manager
        .request_restart(&id, None, true, None, None)
        .await
        .unwrap();
    manager
        .fail_restart(&id, "automatic restore failed".into())
        .await;
    assert!(failed.await.unwrap().is_err());
    let error = manager.join_restart(&id, None, None).await.unwrap_err();
    assert_eq!(format!("{error}"), "automatic restore failed");
    assert!(!manager.restart_requested(&id).await);
}

#[tokio::test]
async fn a_late_join_of_a_cancelled_restart_fails() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let cancelled = manager
        .request_restart(&id, None, true, None, None)
        .await
        .unwrap();
    manager
        .executions
        .with_agent(&id, |agent| agent.cancel_recovery())
        .await;
    // The request in progress learns it too.
    assert!(cancelled.await.is_err());
    manager.join_restart(&id, None, None).await.unwrap_err();
}

#[tokio::test]
async fn a_request_of_a_run_that_restarted_the_agent_already_leaves_it_running() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarted = manager
        .request_restart(&id, Some("run".into()), true, None, None)
        .await
        .unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    restarted.await.unwrap().unwrap();

    // A later request of the same run, such as from another command that
    // continues it, finds the agent restarted and does not stop it.
    manager
        .restart_agent(&id, Some("run".into()), true, None, None)
        .await
        .unwrap();
    assert!(!manager.restart_requested(&id).await);
    assert!(!*execution.stop.borrow());
    // Another run restarts it again, and a late request of the first run
    // neither joins that restart nor, once it ended, starts another.
    let again = manager
        .request_restart(&id, Some("other".into()), true, None, None)
        .await
        .unwrap();
    assert!(manager.restart_requested(&id).await);
    manager
        .restart_agent(&id, Some("run".into()), true, None, None)
        .await
        .unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    again.await.unwrap().unwrap();
    manager
        .restart_agent(&id, Some("run".into()), true, None, None)
        .await
        .unwrap();
    assert!(!manager.restart_requested(&id).await);
    assert!(!*execution.stop.borrow());
}

#[tokio::test]
async fn a_late_join_answers_for_the_restart_of_its_run_while_another_runs() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let first = manager
        .request_restart(&id, Some("first".into()), true, None, None)
        .await
        .unwrap();
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    first.await.unwrap().unwrap();
    // Another command restarts the agent for its own run.
    let second = manager
        .request_restart(&id, Some("second".into()), true, None, None)
        .await
        .unwrap();

    // The first run's late join finds its restart ended well, and does not
    // wait for the second.
    manager
        .join_restart(&id, Some("first".into()), Some("first".into()))
        .await
        .unwrap();
    // The second run's join waits for its own, which then fails.
    let joined = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move {
            manager
                .join_restart(&id, Some("second".into()), Some("second".into()))
                .await
        }
    });
    manager.fail_restart(&id, "cancelled".into()).await;
    assert!(second.await.unwrap().is_err());
    let error = joined.await.unwrap().unwrap_err();
    assert_eq!(format!("{error}"), "cancelled");
    // The first run's outcome stays its own.
    manager
        .join_restart(&id, Some("first".into()), Some("first".into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_join_counts_the_restart_for_the_run_that_continues() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let restarted = manager
        .request_restart(&id, Some("b".into()), true, None, None)
        .await
        .unwrap();
    // A run continuing as `a` joins the restart `b` asked for, before or
    // after it ends; either way it counts for `a`.
    let joined = tokio::spawn({
        let manager = manager.clone();
        let id = id.clone();
        async move {
            manager
                .join_restart(&id, Some("b".into()), Some("a".into()))
                .await
        }
    });
    assert!(manager.resume_agent_execution(&id, None).await.unwrap());
    manager.agent_started(&id).await;
    restarted.await.unwrap().unwrap();
    joined.await.unwrap().unwrap();
    // Joining it once it ended counts it for that run as well.
    manager
        .join_restart(&id, Some("b".into()), Some("c".into()))
        .await
        .unwrap();
    let listed = manager.running_agents().await.unwrap();
    assert_eq!(listed[0].restarted_by, ["a", "b", "c"]);
}

/// Hand the restart of the agent `id` runs over to a new wrapper, as its
/// wrapper does once the agent's processes stopped, and end its execution.
/// Returns the pending restart.
async fn hand_over(
    manager: &std::sync::Arc<Manager>,
    workspace: &Workspace,
    execution: &crate::daemon::workspace::StartedExecution,
) -> tokio::task::JoinHandle<anyhow::Result<Option<Origin>>> {
    let id = &execution.plan.id;
    track(manager, workspace, id, "claude", true).await;
    let restarting = tokio::spawn({
        let (manager, id) = (manager.clone(), id.to_owned());
        async move { manager.restart_agent(&id, None, false, None, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    manager.hand_over_restart(id).await.unwrap();
    // A stop sees the restart before the execution's record goes, so no
    // moment shows it neither.
    assert!(manager.renewing_workspaces().await.contains(&workspace.id));
    let complete = manager
        .finish_execution(id.to_owned(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    assert!(complete);
    restarting
}

/// How the restart `restarting` ended; the limit is a hang guard only.
async fn answered(
    restarting: tokio::task::JoinHandle<anyhow::Result<Option<Origin>>>,
) -> anyhow::Result<Option<Origin>> {
    tokio::time::timeout(Duration::from_secs(60), restarting)
        .await
        .expect("the restart was never answered")
        .unwrap()
}

/// The restores waiting in the agent queue for `execution`.
async fn restores_of(manager: &Manager, execution: &str) -> usize {
    let entries = manager.queue_entries().await.unwrap();
    entries
        .iter()
        .filter(|entry| entry.execution.as_deref() == Some(execution))
        .count()
}

/// A stop cancels a restart between its wrappers, whether it comes before
/// the restart records the new wrapper's start or after, and leaves the
/// agent's record for `shoal resume`.
#[tokio::test]
async fn a_stop_cancels_a_restart_between_its_wrappers() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let begin = || manager.begin_execution(&workspace.id, None, ExecutionKind::Command, None);
    let first = begin().await.unwrap();
    let before = first.plan.id.clone();

    let restarting = hand_over(&manager, &workspace, &first).await;
    // `shoal stop --all` selects the workspace, though no execution or
    // queue entry runs or starts the agent now.
    let targets = manager
        .stop_targets(crate::protocol::QueuedLaunches::Cancel)
        .await
        .unwrap();
    assert!(targets.iter().any(|target| target.id == workspace.id));
    manager
        .cancel_stopped_restores(&workspace.id)
        .await
        .unwrap();
    let error = answered(restarting).await.unwrap_err();
    assert!(format!("{error}").contains("cancelled"), "{error}");
    manager
        .restart_in_new_wrapper(&workspace, &before, true)
        .await;
    assert_eq!(restores_of(&manager, &before).await, 0);

    let second = begin().await.unwrap();
    let after = second.plan.id.clone();
    let restarting = hand_over(&manager, &workspace, &second).await;
    manager
        .restart_in_new_wrapper(&workspace, &after, true)
        .await;
    assert_eq!(restores_of(&manager, &after).await, 1);
    manager
        .cancel_stopped_restores(&workspace.id)
        .await
        .unwrap();
    let error = answered(restarting).await.unwrap_err();
    assert!(format!("{error}").contains("cancelled"), "{error}");
    assert_eq!(restores_of(&manager, &after).await, 0);

    for id in [&before, &after] {
        assert!(recovery::record_path(&manager.paths, &workspace.id, id).is_file());
    }
}

#[tokio::test]
async fn a_narrowing_restarts_an_agent_only_in_a_new_wrapper_and_only_its_own_restart() {
    let (root, manager) = manager().await;
    let workspace = workspace(root.path(), &manager).await;
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&manager, &workspace, &id, "claude", true).await;
    let narrowing = |assignment: &str| {
        Some(crate::config::permissions::PermissionsConfig::assigned(&[assignment.into()]).unwrap())
    };
    // The wrapper that restarts its agent in place keeps the narrowing it
    // launched it with.
    assert!(
        manager
            .request_restart(&id, None, true, None, narrowing("push=propose"))
            .await
            .is_err()
    );
    assert!(!manager.restart_requested(&id).await);

    let _first = manager
        .request_restart(
            &id,
            Some("first".into()),
            false,
            None,
            narrowing("push=propose"),
        )
        .await
        .unwrap();
    // Another run joins it under the same narrowing, or without one, but
    // not under another.
    let other = manager
        .request_restart(
            &id,
            Some("other".into()),
            false,
            None,
            narrowing("push=deny"),
        )
        .await;
    assert!(other.is_err());
    for same in [None, narrowing("push=propose")] {
        manager
            .request_restart(&id, Some("same".into()), false, None, same)
            .await
            .unwrap();
    }
}
