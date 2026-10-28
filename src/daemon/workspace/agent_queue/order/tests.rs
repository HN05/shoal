use super::*;

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn entries_move_to_a_position_in_front_of_an_entry_or_last() {
    let queue = ids(&["a", "b", "c", "d"]);
    let moved = |entry, place| reorder(&queue, entry, &place).unwrap();
    assert_eq!(moved("d", QueuePlace::Position(1)), ["d", "a", "b", "c"]);
    assert_eq!(moved("a", QueuePlace::Position(3)), ["b", "c", "a", "d"]);
    assert_eq!(moved("b", QueuePlace::Position(4)), ["a", "c", "d", "b"]);
    assert_eq!(moved("a", QueuePlace::Last), ["b", "c", "d", "a"]);
    // In front of an entry, whether it is ahead of or behind the moved one.
    assert_eq!(
        moved("a", QueuePlace::Before("d".into())),
        ["b", "c", "a", "d"]
    );
    assert_eq!(
        moved("d", QueuePlace::Before("b".into())),
        ["a", "d", "b", "c"]
    );
}

#[test]
fn positions_outside_the_queue_are_refused() {
    let queue = ids(&["a", "b"]);
    for position in [0, 3] {
        let error = reorder(&queue, "a", &QueuePlace::Position(position)).unwrap_err();
        assert!(error.to_string().contains("choose 1 to 2 or last"));
    }
    assert!(reorder(&queue, "a", &QueuePlace::Before("gone".into())).is_err());
}
