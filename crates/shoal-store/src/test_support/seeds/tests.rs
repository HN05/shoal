use std::path::Path;

use anyhow::Result;
use shoal_protocol::{model::SpaceKind, state::ExecutionState, test_support};

use super::*;
use crate::{spaces, test_support::migrated};

#[test]
fn seeded_spaces_read_back_as_their_records() -> Result<()> {
    let db = migrated()?;
    repository(&db, &test_support::repository("repo", Path::new("/repo")))?;
    let mut seeded = test_support::workspace("work", "repo", "feature", Path::new("/work"));
    seeded.mode = Some("worker".parse()?);
    seeded.number = workspace(&db, &seeded)?;
    let mut talk = test_support::chat("talk", None, "chat", Path::new("/root"));
    talk.number = chat(&db, &talk)?;
    parent(&db, &seeded.id, &talk.id)?;

    let read = spaces::find_workspace(&db, &seeded.id)?.expect("the workspace is recorded");
    assert_eq!(serde_json::to_value(read)?, serde_json::to_value(&seeded)?);
    assert_ne!(seeded.number, talk.number);
    let facts = spaces::facts(&db, &talk.id)?.expect("the chat is recorded");
    assert_eq!(facts.kind, SpaceKind::Chat);
    let parent: SpaceId = db.query_row(
        "SELECT parent_id FROM space_parents WHERE space_id=?1",
        [&seeded.id],
        |row| row.get(0),
    )?;
    assert_eq!(parent, talk.id);

    let mut run = test_support::execution("run", "work");
    run.state = ExecutionState::Unknown;
    run.agent = Some("claude".into());
    run.mode = seeded.mode.clone();
    execution(&db, &run, None)?;
    let read = crate::find_execution(&db, &run.id)?.expect("the execution is recorded");
    assert_eq!(serde_json::to_value(read)?, serde_json::to_value(&run)?);
    Ok(())
}

#[test]
fn seeded_queue_entries_of_each_recorded_kind_meet_the_schema() -> Result<()> {
    let db = migrated()?;
    let workspace = ready_workspace(&db, "repo", "work", "feature")?;
    let job = serde_json::from_value(serde_json::json!({
        "job": {"handle": "h", "id": 1, "run": 1, "name": "rust", "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/1", "workflow": null, "commit": "abc", "trigger": "other"},
    }))?;
    let entries = [
        QueueEntry::new("launch", QueueKind::Launch, QueueState::Queued),
        QueueEntry {
            space: Some(workspace.id.clone()),
            execution: Some("stopped".into()),
            automatic: true,
            ..QueueEntry::new("restore", QueueKind::Restore, QueueState::Queued)
        },
        QueueEntry {
            repository: Some("repo".into()),
            job: Some(job),
            ..QueueEntry::new("job", QueueKind::Job, QueueState::Starting)
        },
    ];
    for entry in &entries {
        queue_entry(&db, entry)?;
    }
    let recorded: Vec<String> = db
        .prepare("SELECT kind FROM agent_queue ORDER BY seq")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    assert_eq!(recorded, ["launch", "restore", "job"]);
    Ok(())
}
