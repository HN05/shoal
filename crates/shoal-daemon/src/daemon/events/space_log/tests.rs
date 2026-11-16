use super::*;
use crate::daemon::test_support::manager;

/// Live workspaces `plan` and `other`, and the journal of `fix`, which
/// was below `plan` until its removal: created, an agent that worked and
/// exited, then removed, recorded in `plan`'s log too.
const JOURNAL: &str = "
    INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
    BEGIN;
    INSERT INTO workspaces(id,path,branch,state)
        VALUES ('plan','/plan','plan','ready'),('other','/other','other','ready');
    INSERT INTO spaces(id,kind,repository_id,name)
        VALUES ('plan','workspace','repo','plan'),('other','workspace','repo','other');
    COMMIT;
    INSERT INTO space_events(space_id,record) VALUES
        ('fix-id',json_object('kind','created','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix')),
        ('fix-id',json_object('kind','agent_started','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix',
            'agent',json_object('name','claude','execution_id','e','exit_code',NULL))),
        ('fix-id',json_object('kind','agent_state','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix',
            'agent_state',json_object('state','working'))),
        ('fix-id',json_object('kind','agent_exited','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix',
            'agent',json_object('name','claude','execution_id','e','exit_code',0))),
        ('fix-id',json_object('kind','removed','space_id','fix-id','space_kind','workspace',
            'name','fix','path','/fix','cause','manual')),
        ('plan',json_object('kind','child_removed','space_id','plan','space_kind','workspace',
            'name','plan','path','/plan',
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
        granted: None,
        read_only: false,
    }
}

#[tokio::test]
async fn a_removed_space_s_log_reads_by_its_id_with_the_default_filter() {
    let (_temp, manager) = manager().await;
    manager
        .store
        .run(|db| Ok(db.execute_batch(JOURNAL)?))
        .await
        .unwrap();
    let fix = SpaceSelector::from("fix-id");
    // A removed space's name selects nothing.
    assert!(
        (manager.space_log(&SpaceSelector::from("fix"), LogFilter::All, 50))
            .await
            .is_err()
    );
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
    let parent = SpaceSelector::from("plan");
    let parent = manager
        .space_log(&parent, LogFilter::Entries, 50)
        .await
        .unwrap();
    assert_eq!(
        kinds(&parent),
        [EventKind::Created, EventKind::ChildRemoved]
    );
    assert!(
        (manager.space_log(&SpaceSelector::from("gone"), LogFilter::All, 50))
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
        let (caller, mut selector) = (caller(caller_space), Some(SpaceSelector::from(space)));
        async move {
            manager
                .narrow_to_log(&caller, &mut selector)
                .await
                .map(|()| selector)
        }
    };
    assert_eq!(
        narrowed("plan", "fix-id").await.unwrap(),
        Some(SpaceSelector::id(&SpaceId::from("fix-id")))
    );
    assert!(narrowed("plan", "plan").await.is_ok());
    assert!(narrowed("other", "fix-id").await.is_err());
    assert!(narrowed("plan", "other").await.is_err());
}

#[test]
fn a_restore_shows_as_restored_rather_than_created_again() {
    let event = |id, kind: &str| {
        serde_json::from_value::<LogEntry>(serde_json::json!({
            "type": "event", "id": id, "created_at": 1, "kind": kind, "space_id": "w",
            "space_kind": "workspace",
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
