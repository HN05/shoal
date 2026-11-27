use super::*;
use crate::removal::UnsavedWork;
use crate::transfer::{Carrying, SshDestination};
use shoal_ids::move_tickets::MoveTicket;
use shoal_protocol::test_support;
use shoal_store::test_support::seeds;
use std::path::Path;

#[test]
fn a_move_round_trips_through_its_columns_and_json() -> Result<()> {
    let mut db = shoal_store::test_support::migrated()?;
    let tx = db.transaction()?;
    seeds::chat(
        &tx,
        &test_support::chat("chat", None, "chat", Path::new("/root")),
    )?;
    let record = MoveRecord {
        space: "chat".into(),
        destination: MoveDestination::Ssh(SshDestination {
            host: "far".into(),
            shoal: None,
            resume: true,
        }),
        ticket: MoveTicket::generate(),
        carrying: Carrying::Subtree,
        unsaved: UnsavedWork::All,
        phase: MovePhase::Packing,
        checkpoints: Default::default(),
        saved_before: Default::default(),
    };
    insert(&tx, &record)?;
    let (destination, kind): (String, String) = tx.query_row(
        "SELECT destination,destination_kind FROM space_moves",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        (destination.as_str(), kind.as_str()),
        (r#"{"host":"far","resume":true}"#, "ssh")
    );
    assert_eq!(read(&tx, &record.space)?, Some(record));
    Ok(())
}
