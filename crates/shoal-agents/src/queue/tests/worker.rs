use crate::{
    model::Space,
    protocol::operations::QueuedLaunch,
    queue::worker::{HasAgent, HasStoppedAgent},
};

use super::*;

/// Queue a launch of the agent in `workspace`, as a worker start does.
async fn queue_worker(fixture: &Fixture, workspace: &Workspace) -> anyhow::Result<()> {
    let request = launch_in(workspace, false);
    (fixture.agents.queue_add(&*fixture.daemon, request))
        .await
        .map(drop)
}

/// The refusal of a second worker, which names the command that frees the
/// workspace.
fn refusal(result: anyhow::Result<()>) -> String {
    format!("{:#}", result.expect_err("a second worker was queued"))
}

/// Replace the stopped or asleep agent of `space` as a replacing launch
/// does, under the queue gate.
async fn replace(fixture: &Fixture, space: &Space) -> anyhow::Result<()> {
    let agents = &fixture.agents;
    let _gate = agents.agent_queue.lock_admission().await;
    let replacement = agents.plan_replacement(space).await?;
    (agents.replace_stopped_worker(&*fixture.daemon, replacement)).await
}

/// Whether `refused` refuses a worker for the stopped agent it would
/// replace.
fn refused_for_stopped_agent(refused: &anyhow::Result<()>) -> bool {
    (refused.as_ref()).is_err_and(|error| error.is::<HasStoppedAgent>())
}

#[tokio::test]
async fn a_running_agent_refuses_another_worker() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("busy").await;
    let _agent = connected_agent(&fixture, &workspace).await;
    let refused = refusal(queue_worker(&fixture, &workspace).await);
    assert!(refused.contains("shoal attach"), "{refused}");
    assert!(live(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_queued_agent_refuses_another_worker() {
    let fixture = Fixture::new().await;
    fixture
        .configure("[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false");
    let workspace = fixture.workspace("waiting").await;
    queue_worker(&fixture, &workspace).await.unwrap();
    let refused = refusal(queue_worker(&fixture, &workspace).await);
    assert!(refused.contains("shoal queue"), "{refused}");
    assert_eq!(live(&fixture).await.len(), 1);
}

/// An agent that waits for its restore holds its workspace's one agent,
/// whether its wrapper or, once that is gone, the daemon restores it, as
/// an asleep agent a launch may replace.
#[tokio::test]
async fn an_agent_waiting_for_its_restore_refuses_another_worker() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("asleep").await;
    let (execution, _stop) = connected_agent(&fixture, &workspace).await;
    assert!(
        fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    (fixture.agents.queue_agent_restore(daemon, &execution))
        .await
        .unwrap();
    let refused = queue_worker(&fixture, &workspace).await;
    assert!(refused_for_stopped_agent(&refused), "{refused:?}");

    // Its wrapper is gone, and the daemon takes over its restore.
    (fixture.agents.store)
        .run(move |db| {
            db.execute("DELETE FROM executions WHERE id=?1", [execution])?;
            Ok(())
        })
        .await
        .unwrap();
    dispatch(&fixture).await;
    let entries = live(&fixture).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let refused = queue_worker(&fixture, &workspace).await;
    assert!(refused_for_stopped_agent(&refused), "{refused:?}");

    // Replacing it takes its restore out of the queue.
    let space = fixture.space(&workspace.id);
    replace(&fixture, &space).await.unwrap();
    assert!(live(&fixture).await.is_empty());
    queue_worker(&fixture, &workspace).await.unwrap();
}

/// An asleep agent whose restore the daemon took over from its wrapper.
async fn asleep_agent(fixture: &Fixture, workspace: &Workspace) {
    let daemon = &*fixture.daemon;
    let (execution, _stop) = connected_agent(fixture, workspace).await;
    assert!(
        fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    (fixture.agents.queue_agent_restore(daemon, &execution))
        .await
        .unwrap();
    (fixture.agents.store)
        .run(move |db| {
            db.execute("DELETE FROM executions WHERE id=?1", [execution])?;
            Ok(())
        })
        .await
        .unwrap();
    dispatch(fixture).await;
}

/// A replacement that a live agent refuses discards nothing, so the asleep
/// agent it would have replaced still waits for its restore.
#[tokio::test]
async fn a_refused_replacement_keeps_the_asleep_agent_s_restore() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("asleep").await;
    asleep_agent(&fixture, &workspace).await;
    let restores = live(&fixture).await;
    assert_eq!(restores.len(), 1, "{restores:?}");
    let _agent = connected_agent(&fixture, &workspace).await;

    let refused = replace(&fixture, &fixture.space(&workspace.id)).await;

    assert!(
        refused.as_ref().is_err_and(|error| error.is::<HasAgent>()),
        "{refused:?}"
    );
    let kept = live(&fixture).await;
    assert_eq!(
        kept.iter().map(|entry| &entry.id).collect::<Vec<_>>(),
        [&restores[0].id]
    );
}

/// An asleep agent whose wrapper waits to restore it runs no turn, so a
/// launch that replaces it stops that wrapper, takes its restore out of
/// the queue and discards it.
#[tokio::test]
async fn replacing_an_asleep_agent_stops_the_wrapper_waiting_for_it() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("asleep").await;
    let (execution, stop) = connected_agent(&fixture, &workspace).await;
    assert!(
        fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    (fixture.agents.queue_agent_restore(daemon, &execution))
        .await
        .unwrap();
    let wrapper = wrapper_ending_when_stopped(&fixture, &execution, stop);

    let request = queued_in(
        &workspace,
        QueuedLaunch {
            replace: true,
            ..claude()
        },
    );
    let admission = fixture.agents.queue_add(daemon, request).await.unwrap();

    wrapper.await.unwrap();
    let entries = live(&fixture).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].id, admission.entry.unwrap().id);
    let paths = &fixture.agents.paths;
    let records = crate::execution::recovery::agent_records(paths, &workspace.id);
    assert!(records.unwrap().is_empty());
}

/// The wrapper of `execution`, which waits to restore its agent after the
/// stop `stop` has seen, ending it once the next stop asks it to, as a real
/// wrapper does.
fn wrapper_ending_when_stopped(
    fixture: &Fixture,
    execution: &ExecutionId,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let (agents, daemon) = (fixture.agents.clone(), fixture.daemon.clone());
    let execution = execution.clone();
    stop.borrow_and_update();
    tokio::spawn(async move {
        stop.changed().await.unwrap();
        (agents.finish_execution(&*daemon, execution, ExecutionKind::Command, Some(0)))
            .await
            .unwrap();
    })
}

/// Record a stopped agent of `workspace` awaiting `shoal resume`,
/// returning its record's path.
fn stopped_agent(fixture: &Fixture, workspace: &Workspace) -> std::path::PathBuf {
    let execution = ExecutionId::from("stopped-agent".to_owned());
    crate::execution::recovery::save_record(
        &fixture.agents.paths,
        &workspace.id,
        &execution,
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap()
}

#[tokio::test]
async fn a_stopped_agent_refuses_another_worker_until_it_is_resumed_or_forgotten() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("stopped").await;
    let record = stopped_agent(&fixture, &workspace);
    let refused = queue_worker(&fixture, &workspace).await;
    assert!(refused_for_stopped_agent(&refused), "{refused:?}");
    std::fs::remove_file(record).unwrap();
    queue_worker(&fixture, &workspace).await.unwrap();
}

#[tokio::test]
async fn replacing_a_stopped_agent_discards_it_and_takes_a_worker() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("stopped").await;
    let record = stopped_agent(&fixture, &workspace);
    let space = fixture.space(&workspace.id);
    replace(&fixture, &space).await.unwrap();
    assert!(!record.exists(), "the stopped agent's record stayed");
    queue_worker(&fixture, &workspace).await.unwrap();
}

/// A start that reopens a workspace, such as on its issue, queues its
/// agent through the start's request, which replaces as a worker's does.
#[tokio::test]
async fn a_start_s_agent_that_replaces_discards_the_stopped_agent() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("reopened").await;
    let record = stopped_agent(&fixture, &workspace);
    let launch = QueuedLaunch {
        replace: true,
        ..QueuedLaunch::new("claude".parse().unwrap())
    };
    let space = fixture.space(&workspace.id);

    let admission = (fixture.agents)
        .queue_start_agent(
            &*fixture.daemon,
            (&space, false),
            launch,
            (None, Vec::new()),
        )
        .await
        .unwrap();

    assert!(admission.entry.is_some());
    assert!(!record.exists(), "the stopped agent's record stayed");
}

/// A replacing launch the queue refuses for another reason, such as a
/// workspace no longer ready, keeps the stopped agent.
#[tokio::test]
async fn a_replacing_launch_the_queue_refuses_keeps_the_stopped_agent() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("failed").await;
    let record = stopped_agent(&fixture, &workspace);
    let failed = WorkspaceState::Failed;
    fixture.set_state(&workspace.id, failed, None).await;

    let replacing = QueuedLaunch {
        replace: true,
        now: true,
        ..claude()
    };
    let request = queued_in(&workspace, replacing);
    let refused = fixture.agents.queue_add(&*fixture.daemon, request).await;

    assert!(
        refused.is_err(),
        "a launch in a failed workspace was queued"
    );
    assert!(record.exists());
}

#[tokio::test]
async fn replacing_refuses_a_running_agent() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("busy").await;
    let _agent = connected_agent(&fixture, &workspace).await;
    let space = fixture.space(&workspace.id);
    let refused = replace(&fixture, &space).await;
    let refused = format!("{:#}", refused.expect_err("a running agent was replaced"));
    assert!(refused.contains("shoal stop"), "{refused}");
}

#[tokio::test]
async fn a_workspace_without_an_agent_takes_a_worker() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("idle").await;
    queue_worker(&fixture, &workspace).await.unwrap();
}

#[tokio::test]
async fn an_agent_without_a_launch_waits_out_one_queued_in_its_workspace() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let repo = fixture.repository().await;
    let workspace = fixture.workspace("worked").await;
    // A selector names a space by a UUID.
    let blocker = SpaceId::generate();
    fixture
        .workspace_with_id(&repo, blocker.as_str(), "blocker")
        .await;
    let selector = SpaceSelector::id(&workspace.id);
    let reserve = async || {
        fixture
            .agents
            .reserve_worker(daemon, &selector)
            .await
            .map(drop)
    };
    // Nothing waits there, so a restored agent may register.
    reserve().await.unwrap();
    // A launch waits there for another space's work.
    let request = QueueRequest {
        blockers: vec![shoal_protocol::progress::Blocker::Space(
            blocker.to_string(),
        )],
        ..launch_in(&workspace, false)
    };
    let admission = fixture.agents.queue_add(daemon, request).await.unwrap();
    let entry = admission.entry.unwrap();
    let refused = reserve().await;
    assert!(
        refused.as_ref().is_err_and(|error| error.is::<HasAgent>()),
        "{refused:?}"
    );
    // Once it leaves the queue, the restore may go ahead.
    (fixture.agents.queue_remove(daemon, entry.id.as_str(), None))
        .await
        .unwrap();
    reserve().await.unwrap();
}
