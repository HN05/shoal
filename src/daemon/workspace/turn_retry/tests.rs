use std::time::Duration;

use anyhow::Context;

use super::*;
use crate::{
    daemon::notifications::NotificationKind,
    model::Workspace,
    test_support::{global_config, manager, repository},
};

#[test]
fn waits_double_from_half_a_minute_to_a_quarter_hour_spread_a_fifth() {
    let failure = TurnFailure::ServerError;
    let waits: Vec<_> = (1..=8)
        .map(|attempt| delay(failure, attempt, 0.5))
        .collect();
    assert_eq!(waits, [30, 60, 120, 240, 480, 900, 900, 900]);
    assert_eq!(delay(failure, 1, 0.0), 24);
    assert_eq!(delay(failure, 1, 0.999_999), 36);
    assert_eq!(delay(failure, u32::MAX, 0.5), 900);
}

#[test]
fn usage_limits_back_off_to_hourly_attempts_without_a_cap() {
    let failure = TurnFailure::RateLimited;
    let waits: Vec<_> = (1..=10)
        .map(|attempt| delay(failure, attempt, 0.5))
        .collect();
    assert_eq!(waits, [30, 60, 120, 240, 480, 960, 1920, 3600, 3600, 3600]);
    assert_eq!(delay(failure, u32::MAX, 0.5), 3600);
    assert_eq!(delay(failure, 10, 0.0), 2880);
    assert_eq!(delay(failure, 10, 0.999_999), 4320);
}

/// A workspace whose connected `claude` agent runs in the returned
/// execution, with `config` as the global config, and the receiver that
/// keeps the agent's connection open.
async fn agent(config: &str) -> Running {
    let (temp, manager) = manager().await;
    manager.publish_config(global_config(config));
    let checkout = repository(temp.path(), "repo");
    manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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
) -> (Workspace, String, tokio::sync::watch::Receiver<bool>) {
    let repo = manager.repositories().await.unwrap().remove(0);
    let workspace = manager
        .create_workspace(&repo.id, name.into(), None, None, None)
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
    (workspace, started.plan.id, started.stop)
}

type Running = (
    tempfile::TempDir,
    std::sync::Arc<Manager>,
    Workspace,
    String,
    tokio::sync::watch::Receiver<bool>,
);

const RETRY_ON: &str = "[agents.claude]\nretry = true\n";

async fn retry(manager: &Manager, workspace: &Workspace) -> Option<TurnRetry> {
    let id = workspace.id.clone();
    manager.store.run(move |db| get(db, &id)).await.unwrap()
}

async fn fail(manager: &Manager, workspace: &Workspace, execution: &str, failure: TurnFailure) {
    let status = manager
        .report_turn_failure(
            &workspace.name,
            failure,
            Some(execution.into()),
            None,
            Default::default(),
        )
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
async fn a_failed_turn_retries_by_default_and_asks_the_idle_agent_to_continue() {
    let (_temp, manager, workspace, execution, _connection) = agent("").await;
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
    assert!(asked[0].contains("(Retry 1.)"), "{asked:?}");
    assert!(retry(&manager, &workspace).await.unwrap().delivered);

    // The turn it starts failing again counts the next attempt.
    report(&manager, &workspace, &execution, AgentState::Working).await;
    assert!(retry(&manager, &workspace).await.is_some());
    fail(&manager, &workspace, &execution, TurnFailure::ServerError).await;
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
    manager
        .mark_done(&workspace.name, false, false, vec![])
        .await
        .unwrap();
    assert_eq!(retry(&manager, &workspace).await, None);
    // Withdrawing completion does not bring the old retry back.
    manager.undo_done(&workspace.name).await.unwrap();
    assert_eq!(deliver_now(&manager).await, None);
    assert!(messages(&manager, &workspace).await.is_empty());
}

#[tokio::test]
async fn a_full_message_queue_defers_only_its_own_retry() {
    let (_temp, manager, full, full_execution, _full) = agent(RETRY_ON).await;
    let (open, open_execution, _open) = another_agent(&manager, "open").await;
    for n in 0..crate::daemon::agent_messages::MAX_UNREAD {
        manager
            .send_agent_message(&full.name, format!("unread {n}"), Vec::new())
            .await
            .unwrap();
    }
    fail(&manager, &full, &full_execution, TurnFailure::Overloaded).await;
    fail(&manager, &open, &open_execution, TurnFailure::Overloaded).await;
    let at = now();
    let next = manager.deliver_turn_retries_at(at + 3600).await.unwrap();
    assert_eq!(messages(&manager, &open).await.len(), 1);
    assert!(retry(&manager, &open).await.unwrap().delivered);
    // The full queue's retry waits the first delay again rather than
    // retrying at once.
    let deferred = retry(&manager, &full).await.unwrap();
    assert!(!deferred.delivered);
    assert_eq!(deferred.due_at, at + 3600 + 30);
    assert_eq!(next, Some(deferred.due_at));
}

/// The messages of the turn failures Shoal told the user of since the last
/// look, marking them read.
async fn turn_failures(manager: &Manager) -> Vec<String> {
    let notifications = manager.notifications(true, 10).await.unwrap();
    let ids = notifications.iter().map(|notification| notification.id);
    manager
        .mark_notifications_read(ids.collect())
        .await
        .unwrap();
    notifications
        .into_iter()
        .filter(|notification| notification.kind == NotificationKind::TurnFailed)
        .map(|notification| notification.message)
        .collect()
}

#[tokio::test]
async fn a_failed_turn_with_retry_off_is_told_once_until_a_turn_goes_through() {
    let (_temp, manager, workspace, execution, _connection) =
        agent("[agents.claude]\nretry = false\n").await;
    fail(&manager, &workspace, &execution, TurnFailure::RateLimited).await;
    assert_eq!(retry(&manager, &workspace).await, None);
    assert!(messages(&manager, &workspace).await.is_empty());
    let told = turn_failures(&manager).await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("(rate limited)"), "{told:?}");
    assert!(
        told[0].contains("`retry = true` under `[agents.claude]`"),
        "{told:?}"
    );

    // Later turns failing, read notification or not, add nothing, and the
    // agent waiting for the user is no turn that went through.
    for state in [AgentState::Working, AgentState::Waiting] {
        report(&manager, &workspace, &execution, state).await;
        fail(&manager, &workspace, &execution, TurnFailure::RateLimited).await;
    }
    report(&manager, &workspace, &execution, AgentState::Idle).await;
    fail(&manager, &workspace, &execution, TurnFailure::Other).await;
    assert_eq!(turn_failures(&manager).await, Vec::<String>::new());

    // Once a turn ends normally, the next failed one is told again.
    report(&manager, &workspace, &execution, AgentState::Working).await;
    report(&manager, &workspace, &execution, AgentState::Idle).await;
    report(&manager, &workspace, &execution, AgentState::Working).await;
    fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
    let told = turn_failures(&manager).await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("(API overloaded)"), "{told:?}");
}

#[tokio::test]
async fn a_failure_no_retry_fixes_is_told_whether_retry_is_on_or_off() {
    for config in ["", "[agents.claude]\nretry = false\n"] {
        let (_temp, manager, workspace, execution, _connection) = agent(config).await;
        fail(&manager, &workspace, &execution, TurnFailure::Other).await;
        assert_eq!(retry(&manager, &workspace).await, None);
        let told = turn_failures(&manager).await;
        assert_eq!(told.len(), 1, "{config}: {told:?}");
        assert!(
            told[0].contains("a retry does not fix"),
            "{config}: {told:?}"
        );
    }
}

#[tokio::test]
async fn an_outage_is_retried_until_it_ends_without_a_cap() {
    let (_temp, manager, workspace, execution, _connection) = agent(RETRY_ON).await;
    for _ in 0..20 {
        fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
        deliver_now(&manager).await;
        report(&manager, &workspace, &execution, AgentState::Working).await;
    }
    fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
    let pending = retry(&manager, &workspace).await.unwrap();
    assert_eq!((pending.attempt, pending.delivered), (21, false));
    assert!(pending.due_at - now() <= 900 * 6 / 5, "{pending:?}");
    assert!(manager.notifications(false, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_retry_waits_past_the_reset_the_upstream_named() {
    let (_temp, manager, workspace, execution, _connection) = agent("").await;
    // Resets far past any clock and long gone, so however slowly the test
    // runs, each wait is the reset's or the backoff's.
    let reset = 4_000_000_000_000;
    // The proxy's report arrives first, as it does for Codex.
    manager
        .report_upstream_reset(&workspace.name, execution.clone(), Some(reset))
        .await
        .unwrap();
    fail(&manager, &workspace, &execution, TurnFailure::RateLimited).await;
    let pending = retry(&manager, &workspace).await.unwrap();
    assert_eq!(pending.due_at, reset + RESET_MARGIN_SECS);
    assert_eq!(deliver_now(&manager).await, None);

    // The agent's own report arrives first, as Claude's may.
    let (other, execution, _other_connection) = another_agent(&manager, "other").await;
    fail(&manager, &other, &execution, TurnFailure::RateLimited).await;
    let early = retry(&manager, &other).await.unwrap();
    assert!(early.due_at < reset, "{early:?}");
    manager
        .report_upstream_reset(&other.name, execution.clone(), Some(reset))
        .await
        .unwrap();
    let held = retry(&manager, &other).await.unwrap();
    assert_eq!(
        (held.attempt, held.due_at),
        (early.attempt, reset + RESET_MARGIN_SECS)
    );
    // A reset that has passed holds nothing back.
    manager
        .report_upstream_reset(&other.name, execution.clone(), Some(0))
        .await
        .unwrap();
    assert_eq!(retry(&manager, &other).await.unwrap().due_at, held.due_at);
    // Once the upstream answers again, a later failure backs off as its own.
    manager
        .report_upstream_reset(&other.name, execution.clone(), None)
        .await
        .unwrap();
    assert_eq!(stored_reset(&manager, &execution).await, None);
    report(&manager, &other, &execution, AgentState::Working).await;
    fail(&manager, &other, &execution, TurnFailure::Overloaded).await;
    let transient = retry(&manager, &other).await.unwrap();
    assert!(transient.due_at < reset, "{transient:?}");
    manager
        .report_upstream_reset(&other.name, execution.clone(), Some(reset))
        .await
        .unwrap();
    assert_eq!(stored_reset(&manager, &execution).await, Some(reset));
    // A wrapper that only lost its connection may come back, and the reset
    // still holds for it; an execution that ended waits for nothing.
    manager.forget_proxy_retry(&execution, false).await;
    assert_eq!(stored_reset(&manager, &execution).await, Some(reset));
    manager.forget_proxy_retry(&execution, true).await;
    assert_eq!(stored_reset(&manager, &execution).await, None);
}

#[tokio::test]
async fn a_lost_connection_keeps_the_reset_and_an_ended_execution_drops_it() {
    let (_temp, manager, workspace, execution, _connection) = agent("").await;
    let reset = 4_000_000_000_000;
    manager
        .report_upstream_reset(&workspace.name, execution.clone(), Some(reset))
        .await
        .unwrap();
    // The wrapper, with its proxy, may reattach.
    let kind = super::super::ExecutionKind::Command;
    manager
        .finish_execution(execution.clone(), kind, None)
        .await
        .unwrap();
    assert_eq!(stored_reset(&manager, &execution).await, Some(reset));
    manager
        .finish_execution(execution.clone(), kind, Some(0))
        .await
        .unwrap();
    assert_eq!(stored_reset(&manager, &execution).await, None);
}

/// The reset `execution`'s proxy reported, as the daemon keeps it.
async fn stored_reset(manager: &Manager, execution: &str) -> Option<i64> {
    let execution = execution.to_owned();
    manager
        .store
        .run(move |db| upstream_reset(db, &execution))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_reset_and_a_failure_hold_the_retry_in_either_order() -> Result<()> {
    let (_temp, manager, workspace, execution, _connection) = agent("").await;
    report(&manager, &workspace, &execution, AgentState::Idle).await;
    let (id, execution) = (workspace.id.clone(), execution.clone());
    let reset = 4_000_000_000_000;
    let due_at = manager
        .store
        .run(move |db| {
            let failure = TurnFailure::RateLimited;
            // Each runs whole: the reset is kept before the failure is
            // recorded, so the retry reads it ...
            record_reset(db, &execution, Some(reset))?;
            record_failure(db, &id, &execution, failure, 1000, 0.5)?;
            let first = get(db, &id)?.context("retry after the reset")?.due_at;
            cancel(db, &id)?;
            record_reset(db, &execution, None)?;
            // ... or the failure is recorded first, and the reset holds it.
            record_failure(db, &id, &execution, failure, 1000, 0.5)?;
            assert!(get(db, &id)?.context("retry")?.due_at < reset);
            assert!(record_reset(db, &execution, Some(reset))?);
            let second = get(db, &id)?.context("held retry")?.due_at;
            Ok::<_, anyhow::Error>((first, second))
        })
        .await?;
    assert_eq!(
        due_at,
        (reset + RESET_MARGIN_SECS, reset + RESET_MARGIN_SECS)
    );
    Ok(())
}

#[tokio::test]
async fn usage_limits_keep_retrying_beyond_a_week_and_survive_reopen() -> Result<()> {
    let (_temp, manager, workspace, execution, _connection) = agent("").await;
    report(&manager, &workspace, &execution, AgentState::Idle).await;
    let id = workspace.id.clone();
    let scheduling_execution = execution.clone();
    let pending = manager
        .store
        .run(move |db| {
            let failure = TurnFailure::RateLimited;
            let mut now = 1000;
            // Drive the scheduling clock, without sleeping through usage limits.
            while now < 1000 + 8 * 24 * 60 * 60 {
                record_failure(db, &id, &scheduling_execution, failure, now, 0.5)?;
                let pending = get(db, &id)?.context("pending usage-limit retry")?;
                assert!(pending.due_at > now);
                assert!(deliver(db, &id, pending.due_at)?);
                db.execute("DELETE FROM agent_messages WHERE workspace_id=?1", [&id])?;
                now = pending.due_at;
            }
            record_failure(db, &id, &scheduling_execution, failure, now, 0.5)?;
            get(db, &id)?.context("retry after eight days")
        })
        .await?;
    assert!(pending.attempt > 8);
    let reopened = Connection::open(manager.paths.database())?;
    assert_eq!(get(&reopened, &workspace.id)?, Some(pending.clone()));
    drop(reopened);
    manager.deliver_turn_retries_at(pending.due_at).await?;
    assert_eq!(
        messages(&manager, &workspace).await,
        [continuation(TurnFailure::RateLimited, pending.attempt)]
    );
    assert!(manager.notifications(false, 10).await?.is_empty());

    // A transient failure after a long usage limit starts its own backoff.
    fail(&manager, &workspace, &execution, TurnFailure::Overloaded).await;
    let transient = retry(&manager, &workspace)
        .await
        .context("transient retry")?;
    assert_eq!(transient.attempt, 1);
    deliver_now(&manager).await;
    fail(&manager, &workspace, &execution, TurnFailure::RateLimited).await;
    let limited = retry(&manager, &workspace)
        .await
        .context("usage-limit retry")?;
    assert_eq!(limited.attempt, 1);
    Ok(())
}
