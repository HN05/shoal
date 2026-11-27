use super::*;
use crate::model::CheckpointCause;
use crate::time::UnixTime;
use shoal_protocol::test_support;
use shoal_store::test_support::seeds;
use std::path::Path;

#[test]
fn a_checkpoint_round_trips_through_its_columns_and_json() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    seeds::repository(&db, &test_support::repository("repo", Path::new("/repo")))?;
    let kept = Checkpoint {
        workspace_id: "workspace".into(),
        repository_id: "repo".into(),
        number: 3.into(),
        name: Some("before".into()),
        cause: CheckpointCause::Manual,
        branch: Some("topic".parse()?),
        head: "a".repeat(40).parse()?,
        commit: "b".repeat(40).parse()?,
        files_omitted: true,
        created_at: UnixTime::from_seconds(1),
        expires_at: UnixTime::from_seconds(2),
    };
    insert(&db, &kept)?;
    let record: String = db.query_row("SELECT record FROM checkpoints", [], |row| row.get(0))?;
    assert!(!record.contains("expires_at"), "{record}");
    let read = db.query_row(
        &format!("SELECT {} FROM checkpoints c", columns("c")),
        [],
        checkpoint,
    )?;
    assert_eq!(read, kept);
    Ok(())
}
