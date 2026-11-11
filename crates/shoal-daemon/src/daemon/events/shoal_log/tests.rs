use super::*;
use crate::daemon::test_support::manager;

/// The live workspace `plan`, and the journal of `fix`, removed by cleanup
/// and restored, between Shoal's own events.
const JOURNAL: &str = "
    INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
    INSERT INTO workspaces(id,repository_id,name,path,branch,state)
        VALUES ('plan','repo','plan','/plan','plan','ready');
    INSERT INTO workspace_events(record) VALUES
        (json_object('kind','daemon_started','version','1.0.0')),
        (json_object('kind','created','workspace_id','fix-id','name','fix')),
        (json_object('kind','agent_state','workspace_id','fix-id','name','fix',
            'agent_state',json_object('state','working'))),
        (json_object('kind','removed','workspace_id','fix-id','name','fix','cause','idle')),
        (json_object('kind','cleanup_ran','removed',json_array('fix'))),
        (json_object('kind','created','workspace_id','fix-id','name','fix')),
        (json_object('kind','restored','workspace_id','fix-id','name','fix')),
        (json_object('kind','config_reloaded','error','expected a table'));";

/// Each entry as its space's name and kind, or Shoal's kind alone.
fn lines(entries: &[LogEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match entry {
            LogEntry::Event(event) => format!("{} {}", event.details.name, event.details.kind),
            LogEntry::Shoal(event) => event.details.kind.to_string(),
            other => panic!("unexpected entry {other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn shoal_s_log_holds_every_space_s_entries_and_shoal_s_own() {
    let (_temp, manager) = manager().await;
    let journal = |db: &mut rusqlite::Connection| Ok(db.execute_batch(JOURNAL)?);
    manager.store.run(journal).await.unwrap();
    let entries = manager.shoal_log(LogFilter::Entries, 50).await.unwrap();
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
    let all = manager.shoal_log(LogFilter::All, 50).await.unwrap();
    let all = lines(&all);
    assert!(all.contains(&"fix agent_state".to_owned()), "{all:?}");
    assert_eq!(all.len(), 10, "{all:?}");
    let newest = manager.shoal_log(LogFilter::Entries, 2).await.unwrap();
    assert_eq!(lines(&newest), ["fix restored", "config_reloaded"]);
}

#[tokio::test]
async fn shoal_s_log_reads_the_newest_entries_across_batches() {
    let (_temp, manager) = manager().await;
    let journal = |db: &mut rusqlite::Connection| {
        Ok(db.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1200)
             INSERT INTO workspace_events(record)
                 SELECT json_object('kind','daemon_started','version',CAST(i AS TEXT)) FROM n;",
        )?)
    };
    manager.store.run(journal).await.unwrap();
    let entries = manager.shoal_log(LogFilter::Entries, 1000).await.unwrap();
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

#[tokio::test]
async fn a_scoped_caller_cannot_read_shoal_s_log() {
    let (_temp, manager) = manager().await;
    let caller = crate::daemon::scope::Caller {
        space_id: "plan".into(),
        execution: None,
    };
    assert!(manager.narrow_to_log(&caller, &mut None).await.is_err());
}
