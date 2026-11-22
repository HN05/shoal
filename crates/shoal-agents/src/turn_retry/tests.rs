use std::time::Duration;

use anyhow::Context;
use shoal_core::paths::StateFiles;
use shoal_daemon_base::alerts::AlertKind;
use shoal_protocol::{agent::Agent, space_names::SpaceSelector};

use super::*;
use crate::{model::Workspace, stand_in::Fixture};

#[test]
fn waits_double_from_half_a_minute_to_a_quarter_hour_spread_a_fifth() {
    let failure = TurnFailure::ServerError;
    let waits: Vec<_> = (1..=8)
        .map(|attempt| delay(failure, attempt, 0.5).as_secs())
        .collect();
    assert_eq!(waits, [30, 60, 120, 240, 480, 900, 900, 900]);
    assert_eq!(delay(failure, 1, 0.0).as_secs(), 24);
    assert_eq!(delay(failure, 1, 0.999_999).as_secs(), 36);
    assert_eq!(delay(failure, u32::MAX, 0.5).as_secs(), 900);
}

#[test]
fn usage_limits_back_off_to_hourly_attempts_without_a_cap() {
    let failure = TurnFailure::RateLimited;
    let waits: Vec<_> = (1..=10)
        .map(|attempt| delay(failure, attempt, 0.5).as_secs())
        .collect();
    assert_eq!(waits, [30, 60, 120, 240, 480, 960, 1920, 3600, 3600, 3600]);
    assert_eq!(delay(failure, u32::MAX, 0.5).as_secs(), 3600);
    assert_eq!(delay(failure, 10, 0.0).as_secs(), 2880);
    assert_eq!(delay(failure, 10, 0.999_999).as_secs(), 4320);
}

/// A workspace whose `claude` agent runs in the returned execution, with
/// `config` as the global config.
async fn agent(config: &str) -> (Fixture, Workspace, ExecutionId) {
    let fixture = Fixture::with_config(config).await;
    let (workspace, execution) = another_agent(&fixture, "agent").await;
    (fixture, workspace, execution)
}

/// Another workspace `name` whose `claude` agent runs in the returned
/// execution.
async fn another_agent(fixture: &Fixture, name: &str) -> (Workspace, ExecutionId) {
    let workspace = fixture.workspace(name).await;
    let claude = Agent::from("claude");
    let execution = fixture.execution(&workspace, name, Some(claude)).await;
    (workspace, execution)
}

const RETRY_ON: &str = "[agents.claude]\nretry = true\n";

async fn retry(fixture: &Fixture, workspace: &Workspace) -> Option<TurnRetry> {
    let id = workspace.id.clone();
    fixture
        .agents
        .store
        .run(move |db| get(db, &id))
        .await
        .unwrap()
}

async fn fail(fixture: &Fixture, workspace: &Workspace, execution: &str, failure: TurnFailure) {
    let status = fixture
        .agents
        .report_turn_failure(
            &fixture.daemon,
            &SpaceSelector::id(&workspace.id),
            failure,
            Some(execution.into()),
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
}

async fn report(fixture: &Fixture, workspace: &Workspace, execution: &str, state: AgentState) {
    fixture
        .agents
        .set_agent_state(
            &fixture.daemon,
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
async fn deliver_now(fixture: &Fixture) -> Option<UnixTime> {
    fixture
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

async fn messages(fixture: &Fixture, workspace: &Workspace) -> Vec<String> {
    fixture
        .agents
        .agent_messages(&fixture.daemon, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.message)
        .collect()
}

#[tokio::test]
async fn a_failed_turn_retries_by_default_and_asks_the_idle_agent_to_continue() {
    let (fixture, workspace, execution) = agent("").await;
    let before = now();
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::Overloaded,
    )
    .await;
    let after = now();
    let pending = retry(&fixture, &workspace).await.unwrap();
    assert_eq!(
        (pending.failure, pending.attempt, pending.delivered),
        (TurnFailure::Overloaded, 1, false)
    );
    assert!(
        (before + secs(24)..=after + secs(36)).contains(&pending.due_at),
        "{pending:?}"
    );
    // A second report of the same failure, such as from a proxy, keeps it.
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::Overloaded,
    )
    .await;
    assert_eq!(retry(&fixture, &workspace).await.unwrap(), pending);
    // Nothing is delivered before it is due.
    let early = fixture
        .agents
        .deliver_turn_retries(pending.due_at - secs(1));
    assert_eq!(early.await.unwrap(), Some(pending.due_at));
    assert!(messages(&fixture, &workspace).await.is_empty());

    let due = fixture.agents.deliver_turn_retries(pending.due_at);
    assert_eq!(due.await.unwrap(), None);
    assert_eq!(
        messages(&fixture, &workspace).await,
        [continuation(TurnFailure::Overloaded, 1)]
    );
    assert!(retry(&fixture, &workspace).await.unwrap().delivered);

    // The turn it starts failing again counts the next attempt.
    report(
        &fixture,
        &workspace,
        execution.as_str(),
        AgentState::Working,
    )
    .await;
    assert!(retry(&fixture, &workspace).await.is_some());
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::ServerError,
    )
    .await;
    let second = retry(&fixture, &workspace).await.unwrap();
    assert_eq!((second.attempt, second.delivered), (2, false));
    deliver_now(&fixture).await;
    // The turn ending normally finishes it.
    report(
        &fixture,
        &workspace,
        execution.as_str(),
        AgentState::Working,
    )
    .await;
    report(&fixture, &workspace, execution.as_str(), AgentState::Idle).await;
    assert_eq!(retry(&fixture, &workspace).await, None);
}

#[tokio::test]
async fn a_new_turn_or_a_busy_agent_cancels_a_retry() {
    let (fixture, workspace, execution) = agent(RETRY_ON).await;
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::ServerError,
    )
    .await;
    // The user's prompt starts a turn before the retry is due.
    report(
        &fixture,
        &workspace,
        execution.as_str(),
        AgentState::Working,
    )
    .await;
    assert_eq!(retry(&fixture, &workspace).await, None);

    // An agent that is no longer idle when the retry is due is not asked.
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::ServerError,
    )
    .await;
    report(
        &fixture,
        &workspace,
        execution.as_str(),
        AgentState::Waiting,
    )
    .await;
    assert_eq!(deliver_now(&fixture).await, None);
    assert_eq!(retry(&fixture, &workspace).await, None);
    assert!(messages(&fixture, &workspace).await.is_empty());
}

#[tokio::test]
async fn a_full_message_queue_defers_only_its_own_retry() {
    let (fixture, full, full_execution) = agent(RETRY_ON).await;
    let (open, open_execution) = another_agent(&fixture, "open").await;
    for n in 0..shoal_daemon_base::alerts::MAX_UNREAD {
        let kind = AlertKind::Message;
        (fixture
            .agents
            .queue_agent_message(&full.id, kind, format!("unread {n}")))
        .await
        .unwrap();
    }
    fail(
        &fixture,
        &full,
        full_execution.as_str(),
        TurnFailure::Overloaded,
    )
    .await;
    fail(
        &fixture,
        &open,
        open_execution.as_str(),
        TurnFailure::Overloaded,
    )
    .await;
    let at = now();
    let next = fixture
        .agents
        .deliver_turn_retries(at + secs(3600))
        .await
        .unwrap();
    assert_eq!(messages(&fixture, &open).await.len(), 1);
    assert!(retry(&fixture, &open).await.unwrap().delivered);
    // The full queue's retry waits the first delay again rather than
    // retrying at once.
    let deferred = retry(&fixture, &full).await.unwrap();
    assert!(!deferred.delivered);
    assert_eq!(deferred.due_at, at + secs(3600 + 30));
    assert_eq!(next, Some(deferred.due_at));
}

/// The messages of the turn failures Shoal told of since the last look.
async fn turn_failures(fixture: &Fixture) -> Vec<String> {
    let told = std::mem::take(&mut *fixture.daemon.notified.lock().unwrap());
    (told.into_iter())
        .filter(|(kind, _)| *kind == AlertKind::TurnFailed)
        .map(|(_, message)| message)
        .collect()
}

#[tokio::test]
async fn a_failed_turn_with_retry_off_is_told_once_until_a_turn_goes_through() {
    let (fixture, workspace, execution) = agent("[agents.claude]\nretry = false\n").await;
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::RateLimited,
    )
    .await;
    assert_eq!(retry(&fixture, &workspace).await, None);
    assert!(messages(&fixture, &workspace).await.is_empty());
    let told = turn_failures(&fixture).await;
    assert_eq!(told.len(), 1, "{told:?}");

    // Later turns failing, read notification or not, add nothing, and the
    // agent waiting for the user is no turn that went through.
    for state in [AgentState::Working, AgentState::Waiting] {
        report(&fixture, &workspace, execution.as_str(), state).await;
        fail(
            &fixture,
            &workspace,
            execution.as_str(),
            TurnFailure::RateLimited,
        )
        .await;
    }
    report(&fixture, &workspace, execution.as_str(), AgentState::Idle).await;
    fail(&fixture, &workspace, execution.as_str(), TurnFailure::Other).await;
    assert_eq!(turn_failures(&fixture).await, Vec::<String>::new());
    // The space's status names the latest failure of the run.
    let id = execution.clone();
    let failure = fixture
        .agents
        .store
        .run(move |db| super::unretried::failure(db, &id))
        .await
        .unwrap();
    assert_eq!(failure, Some(TurnFailure::Other));

    // Once a turn ends normally, the next failed one is told again.
    report(
        &fixture,
        &workspace,
        execution.as_str(),
        AgentState::Working,
    )
    .await;
    report(&fixture, &workspace, execution.as_str(), AgentState::Idle).await;
    report(
        &fixture,
        &workspace,
        execution.as_str(),
        AgentState::Working,
    )
    .await;
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::Overloaded,
    )
    .await;
    let told = turn_failures(&fixture).await;
    assert_eq!(told.len(), 1, "{told:?}");
}

#[tokio::test]
async fn a_failure_no_retry_fixes_is_told_with_retry_on() {
    let (fixture, workspace, execution) = agent("").await;
    fail(&fixture, &workspace, execution.as_str(), TurnFailure::Other).await;
    assert_eq!(retry(&fixture, &workspace).await, None);
    let told = turn_failures(&fixture).await;
    assert_eq!(told.len(), 1, "{told:?}");
}

#[tokio::test]
async fn an_outage_is_retried_until_it_ends_without_a_cap() {
    let (fixture, workspace, execution) = agent(RETRY_ON).await;
    for _ in 0..20 {
        fail(
            &fixture,
            &workspace,
            execution.as_str(),
            TurnFailure::Overloaded,
        )
        .await;
        deliver_now(&fixture).await;
        report(
            &fixture,
            &workspace,
            execution.as_str(),
            AgentState::Working,
        )
        .await;
    }
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::Overloaded,
    )
    .await;
    let pending = retry(&fixture, &workspace).await.unwrap();
    assert_eq!((pending.attempt, pending.delivered), (21, false));
    assert!(
        pending.due_at.duration_since(now()) <= secs(900 * 6 / 5),
        "{pending:?}"
    );
    assert!(fixture.daemon.notified.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_reset_and_a_failure_hold_the_retry_in_either_order() -> Result<()> {
    let (fixture, workspace, execution) = agent("").await;
    report(&fixture, &workspace, execution.as_str(), AgentState::Idle).await;
    let (id, execution) = (workspace.id.clone(), execution.clone());
    let reset = UnixTime::from_seconds(4_000_000_000_000);
    let due_at = fixture
        .agents
        .store
        .write(move |tx| {
            let failure = TurnFailure::RateLimited;
            // Each runs whole: the reset is kept before the failure is
            // recorded, so the retry reads it ...
            record_reset(tx, &execution, Some(reset))?;
            record_failure(
                tx,
                &id,
                &execution,
                failure,
                UnixTime::from_seconds(1000),
                0.5,
            )?;
            let first = get(tx, &id)?.context("retry after the reset")?.due_at;
            cancel(tx, &id)?;
            record_reset(tx, &execution, None)?;
            // ... or the failure is recorded first, and the reset holds it.
            record_failure(
                tx,
                &id,
                &execution,
                failure,
                UnixTime::from_seconds(1000),
                0.5,
            )?;
            assert!(get(tx, &id)?.context("retry")?.due_at < reset);
            assert!(record_reset(tx, &execution, Some(reset))?);
            let second = get(tx, &id)?.context("held retry")?.due_at;
            Ok::<_, anyhow::Error>((first, second))
        })
        .await?;
    assert_eq!(due_at, (reset + RESET_MARGIN, reset + RESET_MARGIN));
    Ok(())
}

#[tokio::test]
async fn usage_limits_keep_retrying_beyond_a_week_and_survive_reopen() -> Result<()> {
    let (fixture, workspace, execution) = agent("").await;
    report(&fixture, &workspace, execution.as_str(), AgentState::Idle).await;
    let id = workspace.id.clone();
    let scheduling_execution = execution.clone();
    let pending = fixture
        .agents
        .store
        .write(move |tx| {
            let failure = TurnFailure::RateLimited;
            let start = UnixTime::from_seconds(1000);
            let mut now = start;
            // Drive the scheduling clock, without sleeping through usage limits.
            while now < start + secs(8 * 24 * 60 * 60) {
                record_failure(tx, &id, &scheduling_execution, failure, now, 0.5)?;
                let pending = get(tx, &id)?.context("pending usage-limit retry")?;
                assert!(pending.due_at > now);
                assert!(deliver(tx, &id, pending.due_at)?);
                tx.execute("DELETE FROM alerts WHERE recipient_id=?1", [&id])?;
                now = pending.due_at;
            }
            record_failure(tx, &id, &scheduling_execution, failure, now, 0.5)?;
            get(tx, &id)?.context("retry after eight days")
        })
        .await?;
    assert!(pending.attempt > 8);
    let reopened = Connection::open(fixture.agents.paths.database())?;
    assert_eq!(get(&reopened, &workspace.id)?, Some(pending.clone()));
    drop(reopened);
    fixture.agents.deliver_turn_retries(pending.due_at).await?;
    assert_eq!(
        messages(&fixture, &workspace).await,
        [continuation(TurnFailure::RateLimited, pending.attempt)]
    );
    assert!(fixture.daemon.notified.lock().unwrap().is_empty());

    // A transient failure after a long usage limit starts its own backoff.
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::Overloaded,
    )
    .await;
    let transient = retry(&fixture, &workspace)
        .await
        .context("transient retry")?;
    assert_eq!(transient.attempt, 1);
    deliver_now(&fixture).await;
    fail(
        &fixture,
        &workspace,
        execution.as_str(),
        TurnFailure::RateLimited,
    )
    .await;
    let limited = retry(&fixture, &workspace)
        .await
        .context("usage-limit retry")?;
    assert_eq!(limited.attempt, 1);
    Ok(())
}
