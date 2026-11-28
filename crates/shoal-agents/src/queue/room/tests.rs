use super::*;

/// A queued entry of `kind`.
fn entry(id: &str, kind: QueueKind) -> QueuedRow {
    QueuedRow {
        id: QueueEntryId::from(id.to_owned()),
        repository: None,
        kind,
        execution: None,
        workspace: None,
        started_here: true,
        automatic: false,
        job: None,
        mode: None,
        agent: "claude".into(),
    }
}

#[test]
fn a_job_starts_no_heavy_agent() {
    let queued = [
        entry("launch", QueueKind::Launch),
        entry("restore", QueueKind::Restore),
        entry("job", QueueKind::Job),
    ];
    let light = Light::default();
    let heavy: Vec<String> = (heavy_entries(&queued, &light))
        .map(|row| row.id.to_string())
        .collect();
    assert_eq!(heavy, ["launch", "restore"]);
}

#[test]
fn a_restore_waits_for_room_unless_its_update_has_not_come() {
    assert!(waits_for_room(Wake::Slot));
    assert!(waits_for_room(Wake::Recovery));
    assert!(waits_for_room(Wake::Update { due: true }));
    assert!(!waits_for_room(Wake::Update { due: false }));
}
