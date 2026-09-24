use super::*;
use serde_json::json;

#[test]
fn creation_requires_created_action_and_canonical_path() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&workspace, &alias).unwrap();
    // Worktrunk 0.78.0: SwitchJsonOutput::from_result and emit_switch_json
    // in src/commands/worktree/switch.rs. The response is a single object.
    let mut fixture = json!({
        "action": "created", "path": alias, "branch": "topic",
        "created_branch": true, "base_branch": "main",
    });
    decode_creation(&fixture.to_string(), &workspace).unwrap();
    for action in ["existing", "already_at", "future_action"] {
        fixture["action"] = json!(action);
        let error = decode_creation(&fixture.to_string(), &workspace).unwrap_err();
        assert!(error.to_string().contains("did not create"));
    }
    fixture["action"] = json!("created");
    fixture["path"] = json!(root.path());
    let error = decode_creation(&fixture.to_string(), &workspace).unwrap_err();
    assert!(error.to_string().contains("unexpected workspace path"));
    fixture["path"] = json!(root.path().join("missing"));
    assert!(decode_creation(&fixture.to_string(), &workspace).is_err());
}

#[test]
fn creation_rejects_missing_malformed_and_non_object_results() {
    let root = tempfile::tempdir().unwrap();
    let valid = json!({"action": "created", "path": root.path()});
    for field in ["action", "path"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(decode_creation(&missing.to_string(), root.path()).is_err());
        for value in [json!(null), json!(true), json!(1), json!([]), json!({})] {
            let mut malformed = valid.clone();
            malformed[field] = value;
            assert!(decode_creation(&malformed.to_string(), root.path()).is_err());
        }
    }
    for malformed in [
        "{".to_owned(),
        "null".into(),
        "false".into(),
        "42".into(),
        r#""string""#.into(),
        "[]".into(),
        json!([valid.clone()]).to_string(),
        json!([valid.clone(), valid]).to_string(),
        json!(["created", root.path()]).to_string(),
    ] {
        assert!(
            decode_creation(&malformed, root.path()).is_err(),
            "{malformed}"
        );
    }
}

#[test]
fn removal_normalizes_single_results_and_preserves_every_outcome() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    // Worktrunk 0.78.0: RemovalPlan::to_json / BranchFate::json_outcome in
    // src/commands/worktree/types.rs; commands/remove.rs emits an array.
    // Keep Shoal's compatibility with a bare result object at this boundary.
    for outcome in [
        "deleted",
        "not_attempted",
        "deferred",
        "retained_unmerged",
        "retained_checked_out",
        "retained_raced",
        "retained_failed",
        "future_outcome",
    ] {
        let fixture = json!({
            "kind": "worktree", "path": missing, "branch": "topic",
            "branch_outcome": outcome, "branch_checked_out_at": null,
        });
        for response in [fixture.clone(), json!([fixture])] {
            let result = decode_removal(&response.to_string(), &missing).unwrap();
            assert_eq!(
                serde_json::to_value(result).unwrap(),
                json!({
                    "removed": true, "branch": "topic", "branch_outcome": outcome,
                    "branch_deleted": outcome == "deleted", "hook_error": null,
                })
            );
        }
    }
    for response in [
        json!({"kind": "branch_only", "pruned": true,
               "branch": "topic", "branch_outcome": "not_attempted"}),
        json!({"branch": null, "branch_outcome": "not_attempted"}),
        json!({"branch_outcome": "not_attempted"}),
    ] {
        let result = decode_removal(&response.to_string(), &missing).unwrap();
        assert_eq!(
            result.branch,
            response["branch"].as_str().map(str::to_owned)
        );
        assert!(!result.branch_outcome.is_deleted());
    }
}

#[test]
fn removal_rejects_invalid_fields_shapes_and_counts() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let valid = json!({"branch": "topic", "branch_outcome": "deleted"});
    for field in ["branch", "branch_outcome"] {
        for value in [
            json!(true),
            json!(1),
            json!([]),
            json!({}),
            json!({"deleted": null}),
        ] {
            let mut malformed = valid.clone();
            malformed[field] = value;
            for response in [malformed.clone(), json!([malformed])] {
                assert!(decode_removal(&response.to_string(), &missing).is_err());
            }
        }
    }
    for malformed in [
        "{".to_owned(),
        "null".into(),
        "false".into(),
        "42".into(),
        r#""string""#.into(),
        "{}".into(),
        r#"{"branch_outcome":null}"#.into(),
        "[{}]".into(),
        "[null]".into(),
        "[[null,\"deleted\"]]".into(),
    ] {
        assert!(decode_removal(&malformed, &missing).is_err(), "{malformed}");
    }
    for response in [json!([]), json!([valid.clone(), valid])] {
        let error = decode_removal(&response.to_string(), &missing).unwrap_err();
        assert!(error.to_string().contains("unexpected number"));
    }
}

#[test]
fn removal_must_be_complete() {
    let root = tempfile::tempdir().unwrap();
    let error = decode_removal(r#"{"branch_outcome":"deleted"}"#, root.path()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("before workspace removal completed")
    );
    assert!(root.path().exists());
}
