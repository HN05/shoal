use serde_json::json;

use super::*;

fn escalate() -> Vec<String> {
    vec!["merging into a release branch".to_owned()]
}

#[test]
fn a_verdict_allows_denies_with_a_reason_or_escalates_quoting_a_case() {
    let allow = json!({"verdict": "allow", "reason": "", "rule": ""});
    assert!(matches!(
        read(allow, &escalate()),
        Ok(Verdict::Allow { .. })
    ));
    let deny = json!({"verdict": "deny", "reason": " Add a type label. ", "rule": ""});
    assert_eq!(
        read(deny, &escalate()),
        Ok(Verdict::Deny {
            reason: "Add a type label.".into()
        })
    );
    let quoted = json!({"verdict": "escalate", "reason": "It targets release/2",
        "rule": "merging into a release branch"});
    assert_eq!(
        read(quoted, &escalate()),
        Ok(Verdict::Escalate {
            reason: "It targets release/2".into(),
            rule: "merging into a release branch".into()
        })
    );
}

#[test]
fn an_escalation_must_quote_a_listed_case_and_a_denial_give_a_reason() {
    let unsure = json!({"verdict": "escalate", "reason": "Not sure", "rule": "unsure"});
    assert_eq!(
        read(unsure.clone(), &escalate()),
        Err(Invalid::Misquoted("unsure".into()))
    );
    // With no cases written down, nothing escalates.
    assert!(matches!(read(unsure, &[]), Err(Invalid::Misquoted(_))));
    for malformed in [
        json!({"verdict": "deny", "reason": " ", "rule": ""}),
        json!({"verdict": "maybe", "reason": "x", "rule": ""}),
        json!({"verdict": "allow", "reason": ""}),
        json!({"verdict": "allow", "reason": "", "rule": "", "extra": 1}),
    ] {
        assert_eq!(
            read(malformed.clone(), &escalate()),
            Err(Invalid::Malformed),
            "{malformed}"
        );
    }
}

#[test]
fn the_schema_requires_every_field_it_names() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let properties = schema["properties"].as_object().unwrap();
    let required = schema["required"].as_array().unwrap();
    assert_eq!(properties.len(), required.len());
}
