use super::*;

fn completion(error: Option<&str>) -> Completion {
    Completion {
        head: "a".repeat(40).parse().unwrap(),
        cleanup: true,
        integrated: false,
        results: Vec::new(),
        summary: Some("found it".into()),
        error: error.map(str::to_owned),
    }
}

#[test]
fn a_completion_round_trips_through_its_columns_and_json() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    let id = SpaceId::from("space");
    let failed = completion(Some("cleanup failed"));
    write(&db, &id, &failed, EventCause::Manual)?;
    let (record, error): (String, Option<String>) =
        db.query_row("SELECT record,error FROM space_completion", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    assert!(!record.contains("error"), "{record}");
    assert_eq!(error.as_deref(), Some("cleanup failed"));
    assert_eq!(read(&db, &id)?, Some(failed));
    Ok(())
}
