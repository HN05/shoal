use serde_json::json;

use super::*;

#[test]
fn rebase_merges_fast_forward_and_can_close_the_source_branch() {
    assert_eq!(
        merge_request("merge".into(), MergeMethod::Rebase, true).body,
        Some(json!({"merge_strategy": "rebase_fast_forward", "close_source_branch": true}))
    );
}
