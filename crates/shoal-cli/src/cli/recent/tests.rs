use super::*;
use crate::client::space_ids::SpaceId;

/// The IDs `texts` hold.
fn ids<const N: usize>(texts: [&str; N]) -> [SpaceId; N] {
    texts.map(SpaceId::from)
}

#[test]
fn previous_skips_the_current_and_missing_spaces() {
    let spaces = ids(["a", "b", "chat"]);
    let spaces: Vec<&SpaceId> = spaces.iter().collect();
    let mut history = History::default();
    for id in ids(["a", "removed", "chat", "ended", "b"]) {
        history.record("attach", &id);
    }
    assert_eq!(history.0["attach"], ["b", "ended", "chat", "removed", "a"]);
    let previous = |current: &[SpaceId]| {
        let current: Vec<&SpaceId> = current.iter().collect();
        history.previous("attach", &spaces, &current)
    };
    assert_eq!(previous(&[]).as_ref().map(SpaceId::as_str), Some("b"));
    assert_eq!(
        previous(&ids(["b"])).as_ref().map(SpaceId::as_str),
        Some("chat")
    );
    assert_eq!(
        previous(&ids(["b", "chat"])).as_ref().map(SpaceId::as_str),
        Some("a")
    );
    let [a, b] = ids(["a", "b"]);
    assert_eq!(
        history
            .previous("attach", &[&a, &b], &[&b])
            .as_ref()
            .map(SpaceId::as_str),
        Some("a")
    );
    assert!(history.previous("cd", &spaces, &[]).is_none());
}

#[test]
fn recording_moves_a_space_to_the_front_and_keeps_the_limit() {
    let mut history = History::default();
    for id in 0..LIMIT + 2 {
        history.record("attach", &SpaceId::from(id.to_string()));
    }
    history.record("attach", &SpaceId::from("5"));
    let ids = &history.0["attach"];
    assert_eq!(ids.len(), LIMIT);
    assert_eq!(ids[0], "5");
    assert_eq!(ids.iter().filter(|id| *id == "5").count(), 1);
}
