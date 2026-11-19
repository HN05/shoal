use serde_json::json;

use super::next_cursor;

#[test]
fn a_connection_names_the_cursor_of_its_next_page() {
    let page = json!({"pageInfo": {"hasNextPage": true, "endCursor": "c1"}});
    assert_eq!(next_cursor(&page).unwrap().as_deref(), Some("c1"));
    let last = json!({"pageInfo": {"hasNextPage": false, "endCursor": null}});
    assert_eq!(next_cursor(&last).unwrap(), None);
    assert!(next_cursor(&json!({"pageInfo": {"hasNextPage": true}})).is_err());
}
