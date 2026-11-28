use std::path::Path;

use super::*;
use crate::store::Store;
use shoal_protocol::events::{
    AgentStateEvent, EventDetails, ShoalEventDetails, ShoalEventKind, SpaceEvent,
};
use shoal_protocol::model::EventCause;
use shoal_protocol::state::AgentState;
use shoal_protocol::test_support;
use shoal_store::{journal, test_support::seeds};

/// Record the live workspace `plan`, and the journal of `fix`, removed by
/// cleanup and restored, between Shoal's own events.
fn journal(db: &mut rusqlite::Connection) -> anyhow::Result<()> {
    seeds::ready_workspace(db, "repo", "plan", "plan")?;
    let shoal = |kind, change: fn(&mut ShoalEventDetails)| {
        let mut details = ShoalEventDetails::new(kind);
        change(&mut details);
        journal::append_shoal(db, &details)
    };
    let fix = |event, cause| {
        let details = EventDetails {
            cause,
            ..test_support::workspace_event("fix-id", "fix", Path::new("/fix"), event)
        };
        seeds::space_event(db, &details).map(drop)
    };
    shoal(ShoalEventKind::DaemonStarted, |details| {
        details.version = Some("1.0.0".into());
    })?;
    fix(created(), None)?;
    let working = AgentStateEvent {
        state: Some(AgentState::Working),
    };
    fix(
        SpaceEvent::AgentState {
            agent_state: working,
        },
        None,
    )?;
    fix(SpaceEvent::Removed, Some(EventCause::Idle))?;
    shoal(ShoalEventKind::CleanupRan, |details| {
        details.removed = vec!["fix".into()];
    })?;
    fix(created(), None)?;
    fix(SpaceEvent::Restored, None)?;
    shoal(ShoalEventKind::ConfigReloaded, |details| {
        details.error = Some("expected a table".into());
    })
}

fn created() -> SpaceEvent {
    SpaceEvent::Created {
        base_ref: None,
        base_workspace: None,
        parent: None,
    }
}

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
        for version in 1..=1200 {
            let mut started = ShoalEventDetails::new(ShoalEventKind::DaemonStarted);
            started.version = Some(version.to_string());
            journal::append_shoal(db, &started)?;
        }
        anyhow::Ok(())
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
