use super::*;
use serde_json::Value;

fn clean_request(workspace: &str, name: &str) -> CleanRequest {
    let mut request = SimRequest::new(name.into());
    (request.clean, request.reason) = (true, Some("first launch".into()));
    CleanRequest {
        workspace_id: workspace.into(),
        workspace_name: "fix".into(),
        repository_id: "repo".into(),
        execution_id: Some("execution".into()),
        requested_at: UnixTime::from_seconds(1),
        updated_at: UnixTime::from_seconds(2),
        attempts: 1,
        request,
        status: CleanRequestStatus::Requested,
        action: Some(CleanAction::CreateAfterEviction),
        simulator_id: Some("sim".into()),
        udid: Some("U".into()),
        apps_removed: Some(3),
        erase_completed: false,
        evicted: vec![EvictedDevice {
            id: "old".into(),
            udid: None,
            installed_apps: Some(1),
        }],
        error: None,
    }
}

#[test]
fn a_clean_request_round_trips_through_its_columns_and_record() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    let mut audit = clean_request("A", "default");
    save(&db, &audit)?;
    (audit.status, audit.attempts) = (CleanRequestStatus::Busy, 2);
    save(&db, &audit)?;

    let found = find(&db, &audit.request.request_id)?.expect("the saved request");
    assert_eq!(serde_json::to_value(&found)?, serde_json::to_value(&audit)?);
    let (status, record): (String, String) = db.query_row(
        "SELECT status,record FROM simulator_clean_requests",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(status, "busy");
    let record: Value = serde_json::from_str(&record)?;
    for moved in ["workspace_id", "status"] {
        assert_eq!(record.get(moved), None, "{record}");
    }
    assert_eq!(record["request"].get("request_id"), None, "{record}");
    Ok(())
}

#[test]
fn history_lists_a_workspace_s_requests_newest_first() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    for (workspace, name) in [("A", "first"), ("B", "other"), ("A", "second")] {
        save(&db, &clean_request(workspace, name))?;
    }
    let names = |entries: Vec<AuditEntry>| -> Vec<String> {
        entries
            .into_iter()
            .map(|e| e.request.request.name)
            .collect()
    };
    let owner = SpaceId::from("A");
    assert_eq!(
        names(history(&db, Some(&owner), 10, None)?),
        ["second", "first"]
    );
    assert_eq!(names(history(&db, None, 10, Some(3))?), ["other", "first"]);
    Ok(())
}

#[test]
fn interrupting_marks_only_requests_in_progress() -> Result<()> {
    let mut db = shoal_store::test_support::migrated()?;
    let mut busy = clean_request("A", "busy");
    busy.status = CleanRequestStatus::Busy;
    let running = clean_request("A", "running");
    save(&db, &busy)?;
    save(&db, &running)?;
    let tx = db.transaction()?;
    interrupt(&tx)?;
    tx.commit()?;
    let status = |audit: &CleanRequest| -> Result<CleanRequestStatus> {
        Ok(find(&db, &audit.request.request_id)?.expect("saved").status)
    };
    assert_eq!(status(&busy)?, CleanRequestStatus::Busy);
    assert_eq!(status(&running)?, CleanRequestStatus::Interrupted);
    Ok(())
}
