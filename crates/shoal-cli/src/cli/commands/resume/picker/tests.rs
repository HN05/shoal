use super::*;

fn stopped(reason: Option<&str>, idle: bool) -> SavedSession {
    SavedSession {
        id: "execution".into(),
        path: "execution.recovery.json".into(),
        agent: Some("claude".into()),
        stopped_at: Some(crate::time::UnixTime::now()),
        stop_reason: reason.map(Into::into),
        idle,
        waits_in_wrapper: false,
    }
}

#[test]
fn agents_are_labelled_by_name_stop_time_and_reason() {
    let label = agent_label(&stopped(Some("critical memory pressure"), false));
    assert!(label.starts_with("claude, stopped "), "{label}");
    assert!(
        label.ends_with(" (critical memory pressure), interrupted"),
        "{label}"
    );
    assert!(!label.contains("execution"), "{label}");
    assert!(agent_label(&stopped(None, true)).ends_with(", idle"));
}
