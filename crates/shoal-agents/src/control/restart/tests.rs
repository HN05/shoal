use crate::config::permissions::PermissionChange;
use crate::control::Requested;
use crate::control::RestartWaits;
use crate::execution::recovery;
use crate::execution::recovery::RecordExt;
use crate::executions::ExecutionKind;
use crate::model::RunningAgent;
use crate::model::Workspace;
use crate::stand_in::Fixture;
use crate::state::AgentState;
use shoal_core::sessions::HostName;
use shoal_core::sessions::Origin;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;
use std::time::Duration;

async fn workspace(fixture: &Arc<Fixture>) -> Workspace {
    fixture.workspace("agents").await
}

async fn track(
    fixture: &Arc<Fixture>,
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

#[tokio::test]
async fn agents_list_connected_agents_and_executions_whose_wrapper_is_gone() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
    let command = fixture
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
    let agent = fixture
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
    let gone = fixture
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
    track(&fixture, &workspace, &agent.plan.id, "claude", true).await;
    fixture.agents.detach_execution(&gone.plan.id).await;
    // A report from the agent is its own state, unlike one from a command.
    fixture
        .agents
        .set_agent_state(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            Some(agent.plan.id.clone()),
            None,
        )
        .await
        .unwrap();

    let mut agents = fixture.agents.running_agents().await.unwrap();
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
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
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
    // Happy records its agents as `happy codex`; `--agent` spells it `happy-codex`.
    track(
        &fixture,
        &workspace,
        &execution.plan.id,
        "happy codex",
        true,
    )
    .await;
    let agents = fixture.agents.running_agents().await.unwrap();
    assert_eq!(
        agents[0].agent.as_ref().map(|agent| agent.name()),
        Some("happy-codex")
    );
}

#[tokio::test]
async fn restart_refuses_agents_it_cannot_restore_without_stopping_them() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
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
    let stopped = fixture
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
    track(&fixture, &workspace, &stopped.plan.id, "claude", true).await;
    fixture
        .agents
        .stop_for_overload(&*fixture.daemon, crate::overload::Pressure::Memory)
        .await;
    assert!(*stopped.stop.borrow());

    fixture
        .agents
        .restart_agent(&custom.plan.id, true, None)
        .await
        .unwrap_err();
    assert!(!*custom.stop.borrow());
    // A refused restart saves no handoff.
    assert!(!recovery::record_path(&fixture.agents.paths, &workspace.id, &custom.plan.id).exists());
    // The agent waits for its restore, which is no stop in progress.
    let refused = fixture
        .agents
        .restart_agent(&stopped.plan.id, true, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("critical memory pressure"), "{refused}");
    assert!(!refused.contains("already stopping"), "{refused}");
    assert!(!fixture.agents.restart_requested(&stopped.plan.id).await);
    fixture
        .agents
        .restart_agent(&ExecutionId::from("unknown"), true, None)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn a_restart_answers_once_the_agent_runs_again_or_with_why_not() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
    let place = Origin::created(HostName::Zmx, "agents".into());
    let execution = fixture
        .agents
        .begin_execution_placed(
            &*fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            None,
            ExecutionKind::Command,
            None,
            crate::executions::ExecutionRequest {
                place: Some(crate::places::ExecutionPlace {
                    origin: place.clone(),
                    agent: true,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    track(&fixture, &workspace, &id, "claude", true).await;

    let restarting = tokio::spawn({
        let fixture = fixture.clone();
        let id = id.clone();
        async move { fixture.agents.restart_agent(&id, true, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    assert!(fixture.agents.restart_requested(&id).await);
    // The handoff is saved before the stop reaches the wrapper.
    let handoff = recovery::Record::load(&recovery::record_path(
        &fixture.agents.paths,
        &workspace.id,
        &id,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(handoff.stage, Some(recovery::Stage::Stopping));
    assert_eq!(
        handoff.stop_reason.as_deref(),
        Some("restarted by shoal restart")
    );
    // A first start that registers only now is not the fixture agent, so
    // the wrapper still receives the restart's stop.
    fixture.agents.agent_started(&id).await;
    assert!(fixture.agents.restart_requested(&id).await);
    // The restore resumes the agent without the queue, and its start answers.
    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, &id, None)
            .await
            .unwrap()
    );
    assert!(!*stop.borrow_and_update());
    assert!(fixture.agents.restart_requested(&id).await);
    fixture.agents.agent_started(&id).await;
    // It answers with the place of the wrapper that runs the agent.
    assert_eq!(restarting.await.unwrap().unwrap(), Some(place));
    assert!(!fixture.agents.restart_requested(&id).await);
    assert!(
        fixture
            .agents
            .notifications
            .list(false, 10)
            .await
            .unwrap()
            .is_empty()
    );

    let restarting = tokio::spawn({
        let fixture = fixture.clone();
        let id = id.clone();
        async move { fixture.agents.restart_agent(&id, true, None).await }
    });
    stop.wait_for(|stopping| *stopping).await.unwrap();
    fixture.agents.fail_restart(&id, "cancelled".into()).await;
    let error = restarting.await.unwrap().unwrap_err();
    assert_eq!(format!("{error}"), "cancelled");
}

#[tokio::test]
async fn a_restart_waiting_for_processes_answers_at_once_and_still_resumes() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
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
        let fixture = fixture.clone();
        let id = id.clone();
        async move { fixture.agents.restart_agent(&id, true, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();

    let issue = "unreadable live process environments (PID 7)";
    fixture
        .agents
        .notify_restore_waits(&*fixture.daemon, &id, issue)
        .await;
    let error = restarting.await.unwrap().unwrap_err();
    assert!(error.downcast_ref::<RestartWaits>().is_some());
    assert!(format!("{error}").contains(issue), "{error}");
    // The restart goes on, and its start no longer has a request to answer.
    assert!(fixture.agents.restart_requested(&id).await);
    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, &id, None)
            .await
            .unwrap()
    );
    fixture.agents.agent_started(&id).await;
    assert!(!fixture.agents.restart_requested(&id).await);
}

#[tokio::test]
async fn a_second_request_joins_the_restart_in_progress() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
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
    let first = tokio::spawn({
        let fixture = fixture.clone();
        let id = id.clone();
        async move { fixture.agents.restart_agent(&id, true, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    // Registered before the agent starts again, which answers it.
    let second = fixture
        .agents
        .begin_restart(&id, true, None)
        .await
        .unwrap()
        .answers()
        .unwrap();
    // The agent's start answers both, and leaves it running.
    assert!(
        fixture
            .agents
            .resume_agent_execution(&*fixture.daemon, &id, None)
            .await
            .unwrap()
    );
    fixture.agents.agent_started(&id).await;
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert!(!fixture.agents.restart_requested(&id).await);
}

#[tokio::test]
async fn a_failed_restart_answers_with_why() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
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
    let failed = fixture
        .agents
        .begin_restart(&id, true, None)
        .await
        .unwrap()
        .answers()
        .unwrap();
    fixture
        .agents
        .fail_restart(&id, "automatic restore failed".into())
        .await;
    let error = failed.await.unwrap().unwrap_err();
    assert_eq!(format!("{error}"), "automatic restore failed");
    assert!(!fixture.agents.restart_requested(&id).await);
}

#[tokio::test]
async fn a_cancelled_restart_answers_that_it_was() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
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
    let cancelled = fixture
        .agents
        .begin_restart(&id, true, None)
        .await
        .unwrap()
        .answers()
        .unwrap();
    fixture
        .agents
        .executions()
        .with_agent(&id, |agent| agent.cancel_recovery())
        .await;
    let error = cancelled.await.unwrap().unwrap_err();
    assert!(format!("{error}").contains("cancelled"), "{error}");
}

#[tokio::test]
async fn a_request_joining_a_waiting_restart_is_answered_with_what_it_waits_for_now() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
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
        let fixture = fixture.clone();
        let id = id.clone();
        async move { fixture.agents.restart_agent(&id, true, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    fixture
        .agents
        .notify_restore_waits(&*fixture.daemon, &id, "PID 7 survives")
        .await;
    restarting.await.unwrap().unwrap_err();
    // Other processes keep it waiting now.
    fixture.agents.restart_waits(&id, "PID 8 survives").await;

    let error = fixture
        .agents
        .restart_agent(&id, true, None)
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<RestartWaits>().is_some());
    assert!(format!("{error}").contains("PID 8 survives"), "{error}");
    assert!(fixture.agents.restart_requested(&id).await);
}

#[tokio::test]
async fn a_newer_request_joins_the_restart_until_it_settles_merging_its_change() {
    let fixture = Arc::new(Fixture::new().await);
    let workspace = workspace(&fixture).await;
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
    let change = |assignments: &[&str]| {
        let changes: Vec<_> = (assignments.iter())
            .map(|assignment| PermissionChange::assigned(assignment).unwrap())
            .collect();
        Some(PermissionChange::combined(&changes))
    };
    let first = change(&["push=propose", "issue_open=propose"]);
    fixture
        .agents
        .begin_restart(&id, false, first)
        .await
        .unwrap();
    // The newer request wins on a key both set, and on how it restarts.
    let joined = fixture
        .agents
        .begin_restart(&id, true, change(&["push=direct"]));
    assert!(matches!(joined.await.unwrap(), Requested::Answers(_)));
    assert_eq!(
        fixture.agents.restart_change(&id).await,
        change(&["issue_open=propose", "push=direct"])
    );
    // Once the wrapper settled it, a newer request waits for the agent to run.
    assert!(fixture.agents.settle_restart(&id).await);
    let later = fixture
        .agents
        .begin_restart(&id, false, None)
        .await
        .unwrap();
    assert!(matches!(later, Requested::Settled));
}

/// Hand the restart of the agent `id` runs over to a new wrapper, as its
/// wrapper does once the agent's processes stopped, and end its execution.
/// Returns the pending restart.
async fn hand_over(
    fixture: &Arc<Fixture>,
    workspace: &Workspace,
    execution: &crate::executions::StartedExecution,
) -> tokio::task::JoinHandle<anyhow::Result<Option<Origin>>> {
    let id = &execution.plan.id;
    track(fixture, workspace, id, "claude", true).await;
    let restarting = tokio::spawn({
        let (agents, id) = (fixture.agents.clone(), id.to_owned());
        async move { agents.restart_agent(&id, false, None).await }
    });
    let mut stop = execution.stop.clone();
    stop.wait_for(|stopping| *stopping).await.unwrap();
    fixture.agents.hand_over_restart(id).await.unwrap();
    // A stop sees the restart before the execution's record goes, so no
    // moment shows it neither.
    let renewing = fixture.agents.renewing_workspaces().await;
    assert!(renewing.contains(&workspace.id));
    let command = ExecutionKind::Command;
    let complete = (fixture.agents)
        .finish_execution(&*fixture.daemon, id.to_owned(), command, Some(0))
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
async fn restores_of(fixture: &Fixture, execution: &str) -> usize {
    let entries = fixture
        .agents
        .queue_entries(&*fixture.daemon)
        .await
        .unwrap();
    entries
        .iter()
        .filter(|entry| entry.execution.as_ref().map(ExecutionId::as_str) == Some(execution))
        .count()
}

/// A stop cancels a restart between its wrappers, whether it comes before
/// the restart records the new wrapper's start or after, and leaves the
/// agent's record for `shoal resume`.
#[tokio::test]
async fn a_stop_cancels_a_restart_between_its_wrappers() {
    let fixture = Arc::new(Fixture::new().await);
    let daemon = &*fixture.daemon;
    let workspace = workspace(&fixture).await;
    let space = fixture.space(&workspace.id);
    let begin = || {
        (fixture.agents).begin_execution(
            daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
    };
    let first = begin().await.unwrap();
    let before = first.plan.id.clone();

    let restarting = hand_over(&fixture, &workspace, &first).await;
    // `shoal stop --all` selects the workspace, though no execution or
    // queue entry runs or starts the agent now.
    let cancel = crate::protocol::QueuedLaunches::Cancel;
    let targets = fixture.agents.stop_targets(daemon, cancel).await.unwrap();
    assert!(targets.iter().any(|target| target.id == workspace.id));
    (fixture
        .agents
        .cancel_stopped_restores(daemon, &workspace.id))
    .await
    .unwrap();
    let error = answered(restarting).await.unwrap_err();
    assert!(format!("{error}").contains("cancelled"), "{error}");
    (fixture
        .agents
        .restart_in_new_wrapper(daemon, &space, &before, true))
    .await;
    assert_eq!(restores_of(&fixture, before.as_str()).await, 0);

    let second = begin().await.unwrap();
    let after = second.plan.id.clone();
    let restarting = hand_over(&fixture, &workspace, &second).await;
    (fixture
        .agents
        .restart_in_new_wrapper(daemon, &space, &after, true))
    .await;
    assert_eq!(restores_of(&fixture, after.as_str()).await, 1);
    (fixture
        .agents
        .cancel_stopped_restores(daemon, &workspace.id))
    .await
    .unwrap();
    let error = answered(restarting).await.unwrap_err();
    assert!(format!("{error}").contains("cancelled"), "{error}");
    assert_eq!(restores_of(&fixture, after.as_str()).await, 0);

    for id in [&before, &after] {
        let paths = &fixture.agents.paths;
        assert!(recovery::record_path(paths, &workspace.id, id).is_file());
    }
}
