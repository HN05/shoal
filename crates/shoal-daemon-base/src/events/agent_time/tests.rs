use super::*;
use crate::store::Store;
use std::time::Duration;

/// A store in a temporary state directory.
async fn store() -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    (temp, store)
}

/// Journal the agent event `kind` of `space` in `repository`, running in
/// `mode`, at the second `at`.
fn journal(db: &Connection, space: &str, repository: &str, kind: &str, mode: &str, at: i64) {
    db.execute(
        "INSERT INTO space_events(created_at,space_id,repository_id,kind,bulk,record)
            VALUES (?5,?1,?2,?3,0,json_object('kind',?3,'space_id',?1,
                'agent',json_object('name','claude','mode',NULLIF(?4,''))))",
        params![space, repository, kind, mode, at],
    )
    .unwrap();
}

/// How long `time` says agents of `repository` ran in `mode`.
fn ran(time: &AgentTime, repository: &str, mode: Option<&str>) -> Option<u64> {
    let key = (RepositoryId::from(repository), mode.map(ModeName::from));
    time.get(&key).map(Duration::as_secs)
}

async fn running(journaled: impl FnOnce(&Connection) + Send + 'static) -> AgentTime {
    let (_temp, store) = store().await;
    store
        .run(move |db| {
            journaled(db);
            running_since(
                db,
                UnixTime::from_seconds(1000),
                UnixTime::from_seconds(2000),
            )
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_agent_runs_from_its_start_to_its_space_s_next_end_or_now() {
    let time = running(|db| {
        journal(db, "a", "repo", "agent_started", "worker", 1100);
        journal(db, "a", "repo", "agent_stopped", "worker", 1300);
        journal(db, "b", "repo", "agent_started", "worker", 1500);
        journal(db, "b", "repo", "agent_exited", "worker", 1600);
        journal(db, "c", "repo", "agent_started", "chat", 1900);
        journal(db, "d", "other", "agent_started", "", 1950);
    })
    .await;
    assert_eq!(ran(&time, "repo", Some("worker")), Some(300));
    assert_eq!(ran(&time, "repo", Some("chat")), Some(100));
    assert_eq!(ran(&time, "other", None), Some(50));
}

#[tokio::test]
async fn an_end_without_a_start_in_the_time_read_ran_from_its_beginning() {
    let time = running(|db| {
        journal(db, "a", "repo", "agent_started", "worker", 900);
        journal(db, "a", "repo", "agent_stopped", "worker", 1200);
        journal(db, "a", "repo", "agent_exited", "worker", 1250);
    })
    .await;
    assert_eq!(ran(&time, "repo", Some("worker")), Some(200));
}
