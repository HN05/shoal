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
    assert!(
        label.ends_with(" after critical memory pressure"),
        "{label}"
    );
    assert!(!label.contains("execution"), "{label}");
}
