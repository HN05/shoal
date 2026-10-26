use super::*;

#[test]
fn github_threads_carry_resolution_comments_and_cursors() {
    let node = json!({"id": "T_1", "isResolved": true, "comments": {
            "pageInfo": {"hasNextPage": true, "endCursor": "c1"},
            "nodes": [{"databaseId": 5}, {"databaseId": 9}]}});
    assert_eq!(
        thread_node(&node).unwrap(),
        GitHubThread {
            node: "T_1".into(),
            resolved: true,
            comments: vec![5, 9],
        }
    );
    assert_eq!(
        next_cursor(&node["comments"]).unwrap().as_deref(),
        Some("c1")
    );
    let last = json!({"pageInfo": {"hasNextPage": false, "endCursor": null}});
    assert_eq!(next_cursor(&last).unwrap(), None);
    assert!(next_cursor(&json!({"pageInfo": {"hasNextPage": true}})).is_err());
    assert!(thread_node(&json!({"id": "T_2"})).is_err());
}

#[test]
fn threads_are_found_from_any_of_their_comments() {
    let threads = [GitHubThread {
        node: "T_1".into(),
        resolved: false,
        comments: vec![5, 9],
    }];
    let found = thread(&threads, 9).unwrap();
    assert_eq!(
        (found.first, found.replies, found.resolved),
        (5, 1, Some(false))
    );
    assert!(thread(&threads, 6).is_none());
}
