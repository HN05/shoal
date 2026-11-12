use super::*;

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn an_entry_moves_in_front_of_another_ahead_of_or_behind_it() {
    let queue = ids(&["a", "b", "c", "d"]);
    assert_eq!(reorder(&queue, "a", "d").unwrap(), ["b", "c", "a", "d"]);
    assert_eq!(reorder(&queue, "d", "b").unwrap(), ["a", "d", "b", "c"]);
    assert!(reorder(&queue, "a", "gone").is_err());
}
