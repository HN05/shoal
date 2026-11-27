use super::*;
use crate::events::{AgentEvent, insert_agent};
use crate::store::Store;
use shoal_protocol::state::ExecutionState;
use shoal_store::test_support::seeds;

/// A store in a temporary state directory.
async fn store() -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    (temp, store)
}

/// Record that the agent of `execution` in the space `space`, of `mode`,
/// started or ended as `kind` says, at the second `at`.
fn journal(db: &Connection, space: &str, kind: EventKind, execution: &str, mode: &str, at: i64) {
    let agent = AgentEvent {
        cause: None,
        source: None,
        name: "claude".into(),
        execution_id: Some(execution.into()),
        mode: Some(mode.into()),
        exit_code: None,
        oom_kills: 0,
    };
    insert_agent(db, &SpaceId::from(space), kind, agent).unwrap();
    db.execute(
        "UPDATE space_events SET created_at=?1 WHERE id=last_insert_rowid()",
        [at],
    )
    .unwrap();
}

/// Record that the execution `id` of the space `space` runs now.
fn runs(db: &Connection, space: &str, id: &str) {
    seeds::bare_execution(db, id, &SpaceId::from(space), ExecutionState::Running).unwrap();
}

/// How long `time` says agents of `repository` ran in `mode`.
fn ran(time: &AgentTime, repository: &str, mode: &str) -> Option<u64> {
    let key = (RepositoryId::from(repository), Some(ModeName::from(mode)));
    time.get(&key).map(Duration::as_secs)
}

/// How long agents ran between the seconds 1000 and 2000, of the agent
/// events `journaled` records in the workspaces `a` and `b` of `repo` and
/// `c` of `other`.
async fn running(journaled: impl FnOnce(&Connection) + Send + 'static) -> AgentTime {
    let (_temp, store) = store().await;
    store
        .run(move |db| {
            seeds::ready_workspace(db, "repo", "a", "a")?;
            seeds::ready_workspace(db, "repo", "b", "b")?;
            seeds::ready_workspace(db, "other", "c", "c")?;
            journaled(db);
            let (since, now) = (UnixTime::from_seconds(1000), UnixTime::from_seconds(2000));
            running_since(db, since, now)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_agent_runs_from_its_start_to_its_end_or_now() {
    use EventKind::{AgentExited, AgentStarted, AgentStopped};
    let time = running(|db| {
        journal(db, "a", AgentStarted, "e1", "worker", 1100);
        journal(db, "a", AgentStopped, "e1", "worker", 1300);
        journal(db, "a", AgentStarted, "e2", "chat", 1900);
        journal(db, "b", AgentStarted, "e3", "worker", 1500);
        journal(db, "b", AgentExited, "e3", "worker", 1600);
        journal(db, "c", AgentStarted, "e4", "worker", 1950);
        runs(db, "a", "e2");
        runs(db, "c", "e4");
    })
    .await;
    assert_eq!(ran(&time, "repo", "worker"), Some(300));
    assert_eq!(ran(&time, "repo", "chat"), Some(100));
    assert_eq!(ran(&time, "other", "worker"), Some(50));
}

#[tokio::test]
async fn an_agent_started_before_the_time_read_counts_from_its_beginning() {
    use EventKind::{AgentStarted, AgentStopped};
    let time = running(|db| {
        journal(db, "a", AgentStarted, "e1", "worker", 900);
        journal(db, "a", AgentStopped, "e1", "worker", 1200);
    })
    .await;
    assert_eq!(ran(&time, "repo", "worker"), Some(200));
}

#[tokio::test]
async fn a_launch_that_ended_before_its_agent_started_ran_for_no_time() {
    use EventKind::{AgentExited, AgentStarted, AgentStopped};
    let time = running(|db| {
        journal(db, "a", AgentStarted, "e1", "worker", 500);
        journal(db, "a", AgentStopped, "e1", "worker", 600);
        journal(db, "a", AgentExited, "e2", "worker", 1500);
    })
    .await;
    assert_eq!(ran(&time, "repo", "worker"), None);
}

#[tokio::test]
async fn an_agent_that_runs_through_the_time_read_counts_all_of_it() {
    let time = running(|db| {
        journal(db, "a", EventKind::AgentStarted, "e1", "worker", 500);
        runs(db, "a", "e1");
    })
    .await;
    assert_eq!(ran(&time, "repo", "worker"), Some(1000));
}

#[tokio::test]
async fn a_start_whose_execution_is_gone_without_an_end_counts_nothing() {
    use EventKind::AgentStarted;
    let time = running(|db| {
        journal(db, "a", AgentStarted, "e1", "worker", 500);
        journal(db, "b", AgentStarted, "e2", "worker", 1500);
    })
    .await;
    assert_eq!(ran(&time, "repo", "worker"), None);
}
