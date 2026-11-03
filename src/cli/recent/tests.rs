use super::*;

#[test]
fn previous_skips_the_current_and_missing_spaces() {
    let spaces = ["a", "b", "chat"];
    let mut history = History::default();
    for id in ["a", "removed", "chat", "ended", "b"] {
        history.record("attach", id);
    }
    assert_eq!(history.0["attach"], ["b", "ended", "chat", "removed", "a"]);
    let previous = |current: &[&str]| history.previous("attach", &spaces, current);
    assert_eq!(previous(&[]).as_deref(), Some("b"));
    assert_eq!(previous(&["b"]).as_deref(), Some("chat"));
    assert_eq!(previous(&["b", "chat"]).as_deref(), Some("a"));
    assert_eq!(
        history.previous("attach", &["a", "b"], &["b"]).as_deref(),
        Some("a")
    );
    assert!(history.previous("cd", &spaces, &[]).is_none());
}

#[test]
fn recording_moves_a_space_to_the_front_and_keeps_the_limit() {
    let mut history = History::default();
    for id in 0..LIMIT + 2 {
        history.record("attach", &id.to_string());
    }
    history.record("attach", "5");
    let ids = &history.0["attach"];
    assert_eq!(ids.len(), LIMIT);
    assert_eq!(ids[0], "5");
    assert_eq!(ids.iter().filter(|id| *id == "5").count(), 1);
}
