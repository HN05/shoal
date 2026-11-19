use serde_json::json;

use super::*;

#[test]
fn rebase_merges_fast_forward_and_can_close_the_source_branch() {
    assert_eq!(
        merge_request("merge".into(), Some(MergeMethod::Rebase), None, true).body,
        Some(json!({"merge_strategy": "rebase_fast_forward", "close_source_branch": true}))
    );
}

#[test]
fn a_merge_without_a_method_takes_the_repositorys_default_strategy() {
    assert_eq!(
        merge_request("merge".into(), None, None, false).body,
        Some(json!({"close_source_branch": false}))
    );
}

#[test]
fn a_squash_merge_sends_its_message() {
    let squash = merge_request(
        "merge".into(),
        Some(MergeMethod::Squash),
        Some("Fix login\n\nWhy\n"),
        false,
    );
    assert_eq!(squash.body.unwrap()["message"], "Fix login\n\nWhy");
}
