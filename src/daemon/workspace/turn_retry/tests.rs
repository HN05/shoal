use std::time::Duration;

use super::*;
use crate::{
    model::Workspace,
    test_support::{global_config, manager, repository},
};

#[test]
fn waits_double_from_half_a_minute_to_a_quarter_hour_spread_a_fifth() {
    let waits: Vec<_> = (1..=ATTEMPTS).map(|attempt| delay(attempt, 0.5)).collect();
    assert_eq!(waits, [30, 60, 120, 240, 480, 900, 900, 900]);
    assert_eq!(delay(1, 0.0), 24);
    assert_eq!(delay(1, 0.999_999), 36);
    assert!(delay(u32::MAX, 0.5) == 900);
}

/// A workspace whose connected `claude` agent runs in the returned
/// execution, with `config` as the global config, and the receiver that
/// keeps the agent's connection open.
async fn agent(config: &str) -> Running {
    let (temp, manager) = manager().await;
    manager.publish_config(global_config(config));
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let started = manager
        .begin_execution(
            &workspace.id,
            None,
            super::super::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .track_agent(
            &started.plan.id,
            "claude",
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    (temp, manager, workspace, started.plan.id, started.stop)
}

type Running = (
    tempfile::TempDir,
    std::sync::Arc<Manager>,
    Workspace,
    String,
    tokio::sync::watch::Receiver<bool>,
);

const RETRY_ON: &str = "[ai.claude]\nretry = true\n";

async fn retry(manager: &Manager, workspace: &Workspace) -> Option<TurnRetry> {
    let id = workspace.id.clone();
    manager.store.run(move |db| get(db, &id)).await.unwrap()
}

async fn fail(manager: &Manager, workspace: &Workspace, execution: &str, failure: TurnFailure) {
    let status = manager
        .report_turn_failure(&workspace.name, failure, Some(execution.into()), None)
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
}

async fn report(manager: &Manager, workspace: &Workspace, execution: &str, state: AgentState) {
    manager
        .set_agent_state(&workspace.name, state, Some(execution.into()), None)
        .await
        .unwrap();
}

/// Make the pending retry due now and deliver it.
/// Deliver every pending retry, however far off it is due.
async fn deliver_now(manager: &Manager) -> Option<i64> {
    manager.deliver_turn_retries_at(i64::MAX).await.unwrap()
}

fn now() -> i64 {
    i64::try_from(crate::time::unix_seconds()).unwrap()
}

async fn messages(manager: &Manager, workspace: &Workspace) -> Vec<String> {
    manager
        .agent_messages(&workspace.name)
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.message)
        .collect()
}

#[tokio::test]
async fn a_failed_turn_waits_then_asks_the_idle_agent_to_continue() {
    let (_temp, manager, workspace, execution, _connection) = agent(RETRY_ON).await;
    let before = now();
    fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
    let after = now();
    let pending = retry(&manager, &workspace).await.unwrap();
    assert_eq!(
        (pending.failure, pending.attempt, pending.delivered),
        (TurnFailure::Overloaded, 1, false)
    );
    assert!(
        (before + 24..=after + 36).contains(&pending.due_at),
        "{pending:?}"
    );
    // A second report of the same failure, such as from a proxy, keeps it.
    fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
    assert_eq!(retry(&manager, &workspace).await.unwrap(), pending);
    // Nothing is delivered before it is due.
    let early = manager.deliver_turn_retries_at(pending.due_at - 1);
    assert_eq!(early.await.unwrap(), Some(pending.due_at));
    assert!(messages(&manager, &workspace).await.is_empty());

    let due = manager.deliver_turn_retries_at(pending.due_at);
    assert_eq!(due.await.unwrap(), None);
    let asked = messages(&manager, &workspace).await;
    assert_eq!(asked.len(), 1);
    assert!(asked[0].contains("(API overloaded)"), "{asked:?}");
    assert!(asked[0].contains("Retry 1 of 8"), "{asked:?}");
    assert!(retry(&manager, &workspace).await.unwrap().delivered);

    // The turn it starts failing again counts the next attempt.
    report(&manager, &workspace, &execution, AgentState::Working).await;
    assert!(retry(&manager, &workspace).await.is_some());
    fail(&manager, &workspace, &execution, TurnFailure::RateLimited).await;
    let second = retry(&manager, &workspace).await.unwrap();
    assert_eq!((second.attempt, second.delivered), (2, false));
    deliver_now(&manager).await;
    // The turn ending normally finishes it.
    report(&manager, &workspace, &execution, AgentState::Working).await;
    report(&manager, &workspace, &execution, AgentState::Idle).await;
    assert_eq!(retry(&manager, &workspace).await, None);
}

#[tokio::test]
async fn a_new_turn_or_a_busy_agent_cancels_a_retry() {
    let (_temp, manager, workspace, execution, _connection) = agent(RETRY_ON).await;
    fail(&manager, &workspace, &execution, TurnFailure::ServerError).await;
    // The user's prompt starts a turn before the retry is due.
    report(&manager, &workspace, &execution, AgentState::Working).await;
    assert_eq!(retry(&manager, &workspace).await, None);

    // An agent that is no longer idle when the retry is due is not asked.
    fail(&manager, &workspace, &execution, TurnFailure::ServerError).await;
    report(&manager, &workspace, &execution, AgentState::Waiting).await;
    assert_eq!(deliver_now(&manager).await, None);
    assert_eq!(retry(&manager, &workspace).await, None);
    assert!(messages(&manager, &workspace).await.is_empty());
}

#[tokio::test]
async fn a_finished_assignment_cancels_its_retry() {
    let (_temp, manager, workspace, execution, _connection) = agent(RETRY_ON).await;
    fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
    manager.mark_done(&workspace.name, false).await.unwrap();
    assert_eq!(retry(&manager, &workspace).await, None);
    // Withdrawing completion does not bring the old retry back.
    manager.undo_done(&workspace.name).await.unwrap();
    assert_eq!(deliver_now(&manager).await, None);
    assert!(messages(&manager, &workspace).await.is_empty());
}

#[tokio::test]
async fn retries_need_the_switch_and_a_failure_a_retry_can_fix() {
    let (_temp, manager, workspace, execution, _connection) = agent("").await;
    fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
    assert_eq!(retry(&manager, &workspace).await, None);

    manager.publish_config(global_config(RETRY_ON));
    fail(&manager, &workspace, &execution, TurnFailure::Other).await;
    assert_eq!(retry(&manager, &workspace).await, None);
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].kind, NotificationKind::TurnFailed);
    assert!(
        notifications[0].message.contains("a retry does not fix"),
        "{}",
        notifications[0].message
    );
}

#[tokio::test]
async fn the_last_failed_attempt_gives_up_and_notifies() {
    let (_temp, manager, workspace, execution, _connection) = agent(RETRY_ON).await;
    for _ in 0..ATTEMPTS {
        fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
        deliver_now(&manager).await;
        report(&manager, &workspace, &execution, AgentState::Working).await;
    }
    assert_eq!(retry(&manager, &workspace).await.unwrap().attempt, ATTEMPTS);
    fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
    assert_eq!(retry(&manager, &workspace).await, None);
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].message.contains("after 8 attempts"),
        "{}",
        notifications[0].message
    );
}
