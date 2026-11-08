use super::*;
use crate::daemon::test_support::manager;

/// Live workspaces `plan` and `other`, and the journal of `fix`, which
/// was below `plan` until its removal: created, an agent that worked and
/// exited, then removed, recorded in `plan`'s log too.
const JOURNAL: &str = "
    INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
    INSERT INTO workspaces(id,repository_id,name,path,branch,state)
        VALUES ('plan','repo','plan','/plan','plan','ready'),
            ('other','repo','other','/other','other','ready');
    INSERT INTO workspace_events(record) VALUES
        (json_object('kind','created','workspace_id','fix-id','name','fix')),
        (json_object('kind','agent_started','workspace_id','fix-id','name','fix',
            'agent',json_object('name','claude','execution_id','e','exit_code',NULL))),
        (json_object('kind','agent_state','workspace_id','fix-id','name','fix',
            'agent_state',json_object('state','working'))),
        (json_object('kind','agent_exited','workspace_id','fix-id','name','fix',
            'agent',json_object('name','claude','execution_id','e','exit_code',0))),
        (json_object('kind','removed','workspace_id','fix-id','name','fix','cause','manual')),
        (json_object('kind','child_removed','workspace_id','plan','name','plan',
            'child',json_object('id','fix-id','name','fix','kind','workspace')));";

fn kinds(entries: &[LogEntry]) -> Vec<EventKind> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            LogEntry::Event(event) => Some(event.details.kind),
            _ => None,
        })
        .collect()
}

fn caller(space: &str) -> Caller {
    Caller {
        space_id: space.into(),
        execution: None,
    }
}

#[tokio::test]
async fn a_removed_space_s_log_reads_by_its_last_name_with_the_default_filter() {
    let (_temp, manager) = manager().await;
    manager
        .store
        .run(|db| Ok(db.execute_batch(JOURNAL)?))
        .await
        .unwrap();
    let fix = SpaceSelector::parse("fix");
    let entries = manager
        .space_log(&fix, LogFilter::Entries, 50)
        .await
        .unwrap();
    assert_eq!(
        kinds(&entries),
        [
            EventKind::Created,
            EventKind::AgentStarted,
            EventKind::AgentExited,
            EventKind::Removed
        ]
    );
    let all = manager.space_log(&fix, LogFilter::All, 50).await.unwrap();
    assert!(kinds(&all).contains(&EventKind::AgentState));
    let newest = manager
        .space_log(&fix, LogFilter::Entries, 2)
        .await
        .unwrap();
    assert_eq!(kinds(&newest), [EventKind::AgentExited, EventKind::Removed]);
    let parent = SpaceSelector::parse("plan");
    let parent = manager
        .space_log(&parent, LogFilter::Entries, 50)
        .await
        .unwrap();
    assert_eq!(
        kinds(&parent),
        [EventKind::Created, EventKind::ChildRemoved]
    );
    assert!(
        (manager.space_log(&SpaceSelector::parse("gone"), LogFilter::All, 50))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_scoped_caller_reads_its_own_log_and_those_of_spaces_below_it() {
    let (_temp, manager) = manager().await;
    manager
        .store
        .run(|db| Ok(db.execute_batch(JOURNAL)?))
        .await
        .unwrap();
    let narrowed = |caller_space: &str, space: &str| {
        let manager = manager.clone();
        let (caller, mut selector) = (caller(caller_space), SpaceSelector::parse(space));
        async move {
            manager
                .narrow_to_log(&caller, &mut selector)
                .await
                .map(|()| selector)
        }
    };
    assert_eq!(
        narrowed("plan", "fix").await.unwrap(),
        SpaceSelector::id("fix-id")
    );
    assert!(narrowed("plan", "plan").await.is_ok());
    assert!(narrowed("other", "fix").await.is_err());
    assert!(narrowed("plan", "other").await.is_err());
}

#[test]
fn a_restore_shows_as_restored_rather_than_created_again() {
    let event = |id, kind: &str| {
        serde_json::from_value::<LogEntry>(serde_json::json!({
            "type": "event", "id": id, "created_at": 1, "kind": kind, "workspace_id": "w",
            "repository_id": "r", "name": "fix", "path": "/fix", "branch": "fix",
            "cause": null, "error": null,
        }))
        .unwrap()
    };
    let events = vec![
        event(1, "created"),
        event(2, "removed"),
        event(3, "created"),
        event(4, "ready"),
        event(5, "restored"),
    ];
    assert_eq!(
        kinds(&shown(events.clone(), LogFilter::Entries, 50)),
        [EventKind::Created, EventKind::Removed, EventKind::Restored]
    );
    assert_eq!(shown(events, LogFilter::All, 50).len(), 5);
}
