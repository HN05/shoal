use serde_json::json;

use super::*;

#[test]
fn merged_commits_need_full_ids() {
    let sha = "a".repeat(40);
    assert_eq!(commit_shas(&[json!({"id": sha})], "id").unwrap(), [sha]);
    for commit in [
        json!({"sha": "abc"}),
        json!({}),
        json!({"sha": "g".repeat(40)}),
    ] {
        assert!(commit_shas(&[commit], "sha").is_err());
    }
}
