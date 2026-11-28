use super::*;
use crate::daemon::scope::Access;
use crate::daemon::{events::EventKind, test_support::manager};
use shoal_protocol::events::{AgentEvent, AgentStateEvent, EventDetails, SpaceEvent};
use shoal_protocol::model::{EventCause, RefPart, SpaceRef};
use shoal_protocol::state::AgentState;
use shoal_protocol::test_support;
use shoal_store::test_support::seeds;
use std::path::Path;

mod journal;

/// Record the live workspaces `plan` and `other`, and the journal of `fix`,
/// which was below `plan` until its removal: created, an agent that worked
/// and exited, then removed, recorded in `plan`'s log too.
fn journal(db: &mut rusqlite::Connection) -> anyhow::Result<()> {
    seeds::ready_workspace(db, "repo", "plan", "plan")?;
    seeds::ready_workspace(db, "repo", "other", "other")?;
    let fix = |event| test_support::workspace_event("fix-id", "fix", Path::new("/fix"), event);
    let agent = |exit_code| {
        Box::new(AgentEvent {
            name: "claude".into(),
            execution_id: Some("e".into()),
            mode: None,
            exit_code,
            oom_kills: 0,
            source: None,
            cause: None,
        })
    };
    let working = AgentStateEvent {
        state: Some(AgentState::Working),
    };
    let removed = EventDetails {
        cause: Some(EventCause::Manual),
        ..fix(SpaceEvent::Removed)
    };
    let child = SpaceRef {
        id: "fix-id".into(),
        name: "fix".into(),
        part: RefPart::Workspace { branch: None },
    };
    let events = [
        fix(SpaceEvent::Created {
            base_ref: None,
            base_workspace: None,
            parent: None,
        }),
        fix(SpaceEvent::AgentStarted { agent: agent(None) }),
        fix(SpaceEvent::AgentState {
            agent_state: working,
        }),
        fix(SpaceEvent::AgentExited {
            agent: agent(Some(0)),
        }),
        removed,
        test_support::workspace_event(
            "plan",
            "plan",
            Path::new("/plan"),
            SpaceEvent::ChildRemoved { child },
        ),
    ];
    for event in &events {
        seeds::space_event(db, event)?;
    }
    Ok(())
}

fn kinds(entries: &[LogEntry]) -> Vec<EventKind> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            LogEntry::Event(event) => Some(event.details.kind()),
            _ => None,
        })
        .collect()
}

fn caller(space: &str) -> Caller {
    Caller {
        space_id: space.into(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    }
}

#[tokio::test]
async fn a_removed_space_s_log_reads_by_its_id_with_the_default_filter() {
    let (_temp, manager) = manager().await;
    manager.store.run(journal).await.unwrap();
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
    manager.store.run(journal).await.unwrap();
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

#[tokio::test]
async fn a_scoped_caller_cannot_read_shoal_s_log() {
    let (_temp, manager) = manager().await;
    let caller = Caller {
        space_id: "plan".into(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    assert!(manager.narrow_to_log(&caller, &mut None).await.is_err());
}
