use super::*;
use serde_json::json;

#[test]
fn branch_outcome_spellings_and_deletion_are_preserved() {
    // Worktrunk 0.78.0: src/commands/worktree/types.rs,
    // BranchFate::json_outcome. Only `deleted` confirms deletion, even though
    // Worktrunk's own BranchFate::deleted also counts the deferred intention.
    let cases = [
        ("deleted", BranchOutcome::Deleted),
        ("not_attempted", BranchOutcome::NotAttempted),
        ("deferred", BranchOutcome::Deferred),
        ("retained_unmerged", BranchOutcome::RetainedUnmerged),
        ("retained_checked_out", BranchOutcome::RetainedCheckedOut),
        ("retained_raced", BranchOutcome::RetainedRaced),
        ("retained_failed", BranchOutcome::RetainedFailed),
        ("retained", BranchOutcome::Retained),
        (
            "future_outcome",
            BranchOutcome::Unknown("future_outcome".into()),
        ),
        ("Deleted", BranchOutcome::Unknown("Deleted".into())),
        ("", BranchOutcome::Unknown("".into())),
    ];
    for (spelling, expected) in cases {
        let outcome: BranchOutcome = serde_json::from_value(json!(spelling)).unwrap();
        assert_eq!(outcome, expected);
        assert_eq!(outcome.to_string(), spelling);
        let deleted = spelling == "deleted";
        assert_eq!(outcome.is_deleted(), deleted);
        let result = RemovalResult {
            removed: true,
            branch: Some("topic".into()),
            branch_outcome: outcome,
            hook_error: Some("post hook failed".into()),
        };
        let wire = json!({
            "removed": true,
            "branch": "topic",
            "branch_deleted": deleted,
            "branch_outcome": spelling,
            "hook_error": "post hook failed",
        });
        assert_eq!(serde_json::to_value(&result).unwrap(), wire);
        let decoded: RemovalResult = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(decoded.branch_outcome, expected);
        assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
    }
}

#[test]
fn deletion_is_derived_from_outcome_on_both_sides_of_the_protocol() {
    for outcome in ["deleted", "deferred", "retained", "future_outcome"] {
        let deleted = outcome == "deleted";
        let wire = json!({
            "removed": true,
            "branch": null,
            "branch_deleted": !deleted,
            "branch_outcome": outcome,
            "hook_error": null,
        });
        let result: RemovalResult = serde_json::from_value(wire).unwrap();
        assert_eq!(result.branch_outcome.is_deleted(), deleted);
        assert_eq!(
            serde_json::to_value(result).unwrap()["branch_deleted"],
            deleted
        );
    }
}

#[test]
fn branch_outcome_requires_a_string() {
    for malformed in [
        json!(null),
        json!(false),
        json!(0),
        json!([]),
        json!({}),
        json!({"deleted": null}),
    ] {
        assert!(serde_json::from_value::<BranchOutcome>(malformed).is_err());
    }
    assert!(serde_json::from_value::<RemovalResult>(json!({"removed": true})).is_err());
}
