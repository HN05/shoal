use super::*;
use serde_json::{Value, json};

/// `target`'s columns, the JSON parsed.
fn stored(target: &ReviewTarget) -> (Option<String>, Value) {
    let (url, json) = columns(target).unwrap();
    (
        url.map(ToString::to_string),
        serde_json::from_str(&json).unwrap(),
    )
}

#[test]
fn a_pr_target_keeps_its_url_in_its_column_alone() {
    let url: ItemRef = "https://forge.example/team/repo/pulls/8".parse().unwrap();
    let target = ReviewTarget::Pr {
        url: url.clone(),
        head: "topic".into(),
        base: "main".into(),
        post: true,
    };
    let (column, json) = stored(&target);
    assert_eq!(column.as_deref(), Some(url.url()));
    assert_eq!(
        json,
        json!({"kind": "pr", "head": "topic", "base": "main", "post": true})
    );
    let (url, json) = columns(&target).unwrap();
    assert_eq!(read(url.cloned(), &json).unwrap(), target);
}

#[test]
fn a_workspace_target_has_no_url() {
    assert_eq!(
        stored(&ReviewTarget::Workspace),
        (None, json!({"kind": "workspace"}))
    );
    assert_eq!(
        read(None, r#"{"kind":"workspace"}"#).unwrap(),
        ReviewTarget::Workspace
    );
}

#[test]
fn a_target_whose_url_does_not_fit_its_kind_is_refused() {
    let url: ItemRef = "https://forge.example/team/repo/pulls/8".parse().unwrap();
    assert!(read(Some(url), r#"{"kind":"workspace"}"#).is_err());
    let pr = r#"{"kind":"pr","head":"topic","base":"main","post":false}"#;
    assert!(read(None, pr).is_err());
    let with_url = r#"{"kind":"pr","url":"u","head":"topic","base":"main","post":false}"#;
    assert!(read(None, with_url).is_err());
}
