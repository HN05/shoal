use super::*;
use crate::store::Store;

/// The live workspace `plan`, and the journal of `fix`, removed by cleanup
/// and restored, between Shoal's own events.
const JOURNAL: &str = "
    INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
    BEGIN;
    INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir) VALUES ('plan','/plan','plan','ready','abc','/admin');
    INSERT INTO spaces(id,kind,repository_id,name) VALUES ('plan','workspace','repo','plan');
    COMMIT;
    INSERT INTO space_events(space_id,kind,bulk,record) VALUES
        (NULL,'daemon_started',0,json_object('kind','daemon_started','version','1.0.0')),
        ('fix-id','created',0,json_object('kind','created','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix')),
        ('fix-id','agent_state',1,json_object('kind','agent_state','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix',
            'agent_state',json_object('state','working'))),
        ('fix-id','removed',0,json_object('kind','removed','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix','cause','idle')),
        (NULL,'cleanup_ran',0,json_object('kind','cleanup_ran','removed',json_array('fix'))),
        ('fix-id','created',0,json_object('kind','created','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix')),
        ('fix-id','restored',0,json_object('kind','restored','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix')),
        (NULL,'config_reloaded',0,json_object('kind','config_reloaded','error','expected a table'));";

/// A store in a temporary state directory.
async fn store() -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    (
        temp,
        shoal_store::test_support::unsynced_store(path)
            .await
            .unwrap(),
    )
}

/// Each entry as its space's name and kind, or Shoal's kind alone.
fn lines(entries: &[LogEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match entry {
            LogEntry::Event(event) => format!("{} {}", event.details.name, event.details.kind()),
            LogEntry::Shoal(event) => event.details.kind.to_string(),
            other => panic!("unexpected entry {other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn shoal_s_log_holds_every_space_s_entries_and_shoal_s_own() {
    let (_temp, store) = store().await;
    let journal = |db: &mut rusqlite::Connection| Ok(db.execute_batch(JOURNAL)?);
    store.run(journal).await.unwrap();
    let entries = read(&store, LogFilter::Entries, 50).await.unwrap();
    assert_eq!(
        lines(&entries),
        [
            "plan created",
            "daemon_started",
            "fix created",
            "fix removed",
            "cleanup_ran",
            "fix restored",
            "config_reloaded",
        ]
    );
    let all = read(&store, LogFilter::All, 50).await.unwrap();
    let all = lines(&all);
    assert!(all.contains(&"fix agent_state".to_owned()), "{all:?}");
    assert_eq!(all.len(), 10, "{all:?}");
    let newest = read(&store, LogFilter::Entries, 2).await.unwrap();
    assert_eq!(lines(&newest), ["fix restored", "config_reloaded"]);
}

#[tokio::test]
async fn shoal_s_log_reads_the_newest_entries_across_batches() {
    let (_temp, store) = store().await;
    let journal = |db: &mut rusqlite::Connection| {
        Ok(db.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1200)
             INSERT INTO space_events(kind,bulk,record)
                 SELECT 'daemon_started',0,json_object('kind','daemon_started','version',CAST(i AS TEXT))
                 FROM n;",
        )?)
    };
    store.run(journal).await.unwrap();
    let entries = read(&store, LogFilter::Entries, 1000).await.unwrap();
    let versions: Vec<_> = (entries.iter())
        .map(|entry| match entry {
            LogEntry::Shoal(event) => event.details.version.clone().unwrap(),
            other => panic!("unexpected entry {other:?}"),
        })
        .collect();
    assert_eq!(versions.len(), 1000);
    assert_eq!(versions.first().map(String::as_str), Some("201"));
    assert_eq!(versions.last().map(String::as_str), Some("1200"));
}
