use super::*;

/// A queued entry of `kind`, `automatic` when Shoal queued it on its own.
fn entry(id: &str, kind: QueueKind, automatic: bool) -> QueuedRow {
    QueuedRow {
        id: QueueEntryId::from(id.to_owned()),
        repository: None,
        kind,
        execution: None,
        workspace: None,
        started_here: true,
        automatic,
        job: None,
        mode: None,
        agent: "claude".into(),
    }
}

#[test]
fn an_asleep_agent_s_restore_waits_for_an_update_rather_than_for_room() {
    let queued = [
        entry("launch", QueueKind::Launch, false),
        entry("resumed", QueueKind::Restore, false),
        entry("asleep", QueueKind::Restore, true),
        entry("job", QueueKind::Job, false),
    ];
    let light = Light::default();
    let waiting: Vec<String> = (heavy_entries(&queued, &light))
        .map(|row| row.id.to_string())
        .collect();
    assert_eq!(waiting, ["launch", "resumed"]);
}
