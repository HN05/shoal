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
