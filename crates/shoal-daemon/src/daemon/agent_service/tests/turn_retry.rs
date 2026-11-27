use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::assignments::Done;
use std::time::Duration;

use shoal_agents::turn_retry::{FailedTurn, RESET_MARGIN, TurnRetry, continuation, get};

use crate::{
    daemon::{
        agents::{Protection, ProtectionExt},
        manager::Manager,
        test_support::{global_config, manager, repository},
        workspace_files::Attacher,
    },
    model::Workspace,
    state::{AgentState, TurnFailure},
    time::UnixTime,
};
use shoal_protocol::space_names::SpaceSelector;

/// A workspace whose connected `claude` agent runs in the returned
/// execution, with `config` as the global config, and the receiver that
/// keeps the agent's connection open.
async fn agent(config: &str) -> Running {
    let (temp, manager) = manager().await;
    manager.publish_config(global_config(config));
    let checkout = repository(temp.path(), "repo");
    manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution, connection) = another_agent(&manager, "agent").await;
    (temp, manager, workspace, execution, connection)
}

/// Another workspace `name` of the repository with a connected `claude`
/// agent, its execution and the receiver keeping its connection open.
async fn another_agent(
    manager: &Manager,
    name: &str,
) -> (Workspace, ExecutionId, tokio::sync::watch::Receiver<bool>) {
    let repo = manager.repositories.list().await.unwrap().remove(0);
    let workspace = manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await
        .unwrap();
    let started = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            crate::daemon::executions::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .agents
        .track_agent(
            manager,
            &started.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    (workspace, started.plan.id, started.stop)
}

type Running = (
    tempfile::TempDir,
    std::sync::Arc<Manager>,
    Workspace,
    ExecutionId,
    tokio::sync::watch::Receiver<bool>,
);

const RETRY_ON: &str = "[agents.claude]\nretry = true\n";

async fn retry(manager: &Manager, workspace: &Workspace) -> Option<TurnRetry> {
    let id = workspace.id.clone();
    manager.store.run(move |db| get(db, &id)).await.unwrap()
}

async fn fail(
    manager: &Manager,
    workspace: &Workspace,
    execution: &str,
    failure: impl Into<FailedTurn>,
) {
    let status = manager
        .agents
        .report_turn_failure(
            manager,
            &SpaceSelector::id(&workspace.id),
            failure.into(),
            Some(execution.into()),
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
}

async fn report(manager: &Manager, workspace: &Workspace, execution: &str, state: AgentState) {
    manager
        .agents
        .set_agent_state(
            manager,
            &SpaceSelector::id(&workspace.id),
            state,
            Some(execution.into()),
            None,
        )
        .await
        .unwrap();
}

/// Make the pending retry due now and deliver it.
/// Deliver every pending retry, however far off it is due.
async fn deliver_now(manager: &Manager) -> Option<UnixTime> {
    manager
        .agents
        .deliver_turn_retries(UnixTime::MAX)
        .await
        .unwrap()
}

fn now() -> UnixTime {
    UnixTime::now()
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

async fn messages(manager: &Manager, workspace: &Workspace) -> Vec<String> {
    manager
        .agents
        .agent_messages(manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.message)
        .collect()
}

#[tokio::test]
async fn a_finished_assignment_cancels_its_retry() {
    let (_temp, manager, workspace, execution, _connection) = agent(RETRY_ON).await;
    fail(
        &manager,
        &workspace,
        execution.as_str(),
        TurnFailure::Overloaded,
    )
    .await;
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(retry(&manager, &workspace).await, None);
    // Withdrawing completion does not bring the old retry back.
    manager
        .spaces
        .undo_space_done(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(deliver_now(&manager).await, None);
    assert!(messages(&manager, &workspace).await.is_empty());
}

#[tokio::test]
async fn a_repository_setting_decides_whether_its_agents_retry() {
    let (_temp, manager, workspace, execution, _connection) =
        agent("[agents.claude]\nretry = false\n").await;
    std::fs::write(workspace.path.join(".shoal.toml"), RETRY_ON).unwrap();
    fail(
        &manager,
        &workspace,
        execution.as_str(),
        TurnFailure::RateLimited,
    )
    .await;
    assert!(retry(&manager, &workspace).await.is_some());
}

#[tokio::test]
async fn a_chats_failed_turn_waits_past_its_reset_until_the_chat_is_done() {
    let (_temp, manager) = manager().await;
    let chat = crate::daemon::test_support::chat(&manager, "talk", "chat", None, None).await;
    let execution = manager.spaces.workspace_executions(&chat.id).await.unwrap()[0]
        .id
        .clone();
    let selector = SpaceSelector::id(&chat.id);
    let reset = UnixTime::from_seconds(4_000_000_000_000);
    manager
        .agents
        .report_upstream_reset(&*manager, &selector, execution.clone(), Some(reset))
        .await
        .unwrap();
    let fail = |failure: TurnFailure| {
        let execution = Some(execution.clone());
        manager.agents.report_turn_failure(
            &*manager,
            &selector,
            failure.into(),
            execution,
            None,
            Default::default(),
            None,
        )
    };
    fail(TurnFailure::RateLimited).await.unwrap();
    let id = chat.id.clone();
    let pending = || {
        let id = id.clone();
        manager.store.run(move |db| get(db, &id))
    };
    assert_eq!(
        pending().await.unwrap().map(|retry| retry.due_at),
        Some(reset + RESET_MARGIN)
    );
    deliver_now(&manager).await;
    let messages: Vec<_> = (manager
        .agents
        .agent_messages(&*manager, &selector)
        .await
        .unwrap()
        .into_iter())
    .map(|message| message.message)
    .collect();
    assert_eq!(messages, [continuation(TurnFailure::RateLimited, 1)]);

    fail(TurnFailure::Overloaded).await.unwrap();
    assert!(pending().await.unwrap().is_some());
    manager
        .spaces
        .mark_space_done(&*manager, &selector, Done::default(), &Attacher::User)
        .await
        .unwrap();
    assert_eq!(pending().await.unwrap(), None);
}

/// Disk space left out of a restore's wait, as the restores of agents
/// asleep while the user is away leave it in their own tests.
const DISK_OFF: &str = "[machine.overload.disk]\nenabled = false\n";

/// End the execution `id`, as once its wrapper leaves without restoring it.
async fn end_execution(manager: &Manager, id: &ExecutionId) {
    let id = id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("DELETE FROM executions WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
}

/// An agent put to sleep while its turn retry waits for a usage limit's
/// reset sleeps for the limit and is restored to continue once the reset
/// passes, its retry kept through the end of its execution.
#[tokio::test]
async fn an_agent_asleep_while_its_usage_limit_stands_is_restored_once_it_resets() {
    let (_temp, manager, workspace, execution, connection) = agent(DISK_OFF).await;
    let selector = SpaceSelector::id(&workspace.id);
    let reset = now() + secs(5 * 60 * 60);
    (manager
        .agents
        .report_upstream_reset(&*manager, &selector, execution.clone(), Some(reset)))
    .await
    .unwrap();
    fail(
        &manager,
        &workspace,
        execution.as_str(),
        TurnFailure::RateLimited,
    )
    .await;
    manager
        .agents
        .stop_idle_agents(&*manager, now() + secs(60))
        .await
        .unwrap();
    assert!(*connection.borrow(), "the idle agent sleeps");
    manager
        .agents
        .queue_agent_restore(&*manager, &execution)
        .await
        .unwrap();
    let id = workspace.id.clone();
    let asleep = (manager.store)
        .read(move |tx| crate::daemon::agent_queue::asleep::current(tx, &id, true))
        .await
        .unwrap()
        .unwrap();
    let limited = Protection::retrying(TurnFailure::RateLimited);
    assert_eq!(
        (asleep.reason, asleep.wakes_when),
        (limited.reason, limited.resumes_when)
    );
    end_execution(&manager, &execution).await;
    let idle = manager
        .agents
        .idle_restores(&*manager, &workspace.id)
        .await
        .unwrap();
    assert!(idle.is_empty(), "its retry keeps the workspace: {idle:?}");

    manager.agents.deliver_turn_retries(reset).await.unwrap();
    assert!(messages(&manager, &workspace).await.is_empty());
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let state = |entries: Vec<crate::model::QueueEntry>| {
        let [entry] = entries.try_into().unwrap();
        entry.state
    };
    let asleep = state(manager.agents.queue_entries(&*manager).await.unwrap());
    assert_eq!(asleep, crate::state::QueueState::Queued);

    let after = manager.agents.deliver_turn_retries(reset + RESET_MARGIN);
    assert_eq!(after.await.unwrap(), None);
    assert_eq!(
        messages(&manager, &workspace).await,
        [continuation(TurnFailure::RateLimited, 1)]
    );
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let restored = state(manager.agents.queue_entries(&*manager).await.unwrap());
    assert_eq!(restored, crate::state::QueueState::Starting);

    // The restored agent's turn failing again backs off further.
    let (restored, _restored_connection) = rerun(&manager, &workspace).await;
    report(&manager, &workspace, restored.as_str(), AgentState::Working).await;
    fail(
        &manager,
        &workspace,
        restored.as_str(),
        TurnFailure::RateLimited,
    )
    .await;
    let again = retry(&manager, &workspace).await.unwrap();
    assert_eq!((again.attempt, again.delivered), (2, false));
}

/// A new execution of the connected `claude` agent in `workspace`, as a
/// restore runs it, and the receiver keeping its connection open.
async fn rerun(
    manager: &Manager,
    workspace: &Workspace,
) -> (ExecutionId, tokio::sync::watch::Receiver<bool>) {
    let started = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            crate::daemon::executions::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .agents
        .track_agent(
            manager,
            &started.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    (started.plan.id, started.stop)
}

mod held_wakes;
