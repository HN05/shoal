use super::*;

#[test]
fn agents_are_labelled_by_name_stop_time_and_reason() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("execution.recovery.json");
    std::fs::write(
        &record,
        r#"{"agent":"claude","stop_reason":"critical memory pressure"}"#,
    )
    .unwrap();
    let label = agent_label(&record);
    assert!(label.starts_with("claude, stopped "), "{label}");
    // A record without a turn counts as interrupted.
    assert!(
        label.ends_with(" (critical memory pressure), interrupted"),
        "{label}"
    );
    assert!(!label.contains("execution"), "{label}");
    std::fs::write(&record, r#"{"agent":"claude","turn":"idle"}"#).unwrap();
    assert!(agent_label(&record).ends_with(", idle"));
}
