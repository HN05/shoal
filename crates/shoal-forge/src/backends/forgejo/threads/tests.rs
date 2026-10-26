use super::*;
use serde_json::json;

#[test]
fn threads_are_found_from_any_of_their_comments() {
    let reviews = [
        (
            json!({"id": 1}),
            json!([{"id": 5, "path": "a.rs", "position": 0, "original_position": 9,
                        "extra_lines_count": 1, "resolver": null},
                    {"id": 8, "path": "a.rs", "position": 0, "original_position": 10, "resolver": {"login": "dev"}},
                    {"id": 6, "path": "a.rs", "position": 10, "original_position": 0, "resolver": null}]),
        ),
        (
            json!({"id": 2}),
            json!([{"id": 9, "path": "a.rs", "position": 0, "original_position": 10, "resolver": null}]),
        ),
    ];
    // Comment 8 ends on the line comment 5's range ends on.
    let found = thread(&reviews, 8).unwrap();
    assert_eq!(
        (found.first, found.replies, found.resolved),
        (5, 1, Some(true))
    );
    assert_eq!(
        found.target,
        Target::Forgejo {
            review: 1,
            path: "a.rs".into(),
            line: -9,
            extra_lines: 1,
        }
    );
    assert_eq!(thread(&reviews, 6).unwrap().replies, 0);
    // Another review's comment on the same line is its own thread.
    let other = thread(&reviews, 9).unwrap();
    assert_eq!((other.first, other.replies), (9, 0));
    assert!(thread(&reviews, 7).is_none());
}

#[test]
fn forgejo_threads_group_by_review_path_and_side() {
    let comments = [
        json!({"id": 4, "path": "a.rs", "position": 3, "original_position": 0, "resolver": null}),
        json!({"id": 2, "path": "a.rs", "position": 3, "original_position": 0, "resolver": {"login": "dev"}}),
        json!({"id": 3, "path": "a.rs", "position": 0, "original_position": 3, "resolver": null}),
    ];
    let listed: Vec<(u64, &Value)> = comments.iter().map(|comment| (1, comment)).collect();
    let threads = threads(&listed);
    assert_eq!(threads[&thread_key(1, &comments[0])], (2, true));
    assert_eq!(threads[&thread_key(1, &comments[2])], (3, false));
    assert!(!threads.contains_key(&thread_key(2, &comments[0])));
}
