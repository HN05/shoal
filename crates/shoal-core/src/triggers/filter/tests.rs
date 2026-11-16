use serde_json::json;

use super::{Condition, failing};

fn condition(toml: &str) -> Condition {
    let condition: Condition = toml::from_str(toml).unwrap();
    condition.validate().unwrap();
    condition
}

#[test]
fn every_condition_must_hold() {
    let input = json!({
        "payload": {
            "action": "opened",
            "issue": { "number": 7 },
            "labels": [{ "name": "bug" }, { "name": "p1" }],
        },
    });
    let holds = |toml: &str| condition(toml).holds(&input);
    assert!(holds("path = 'payload.action'\nequals = 'opened'"));
    assert!(!holds("path = 'payload.action'\nequals = 'closed'"));
    assert!(holds("path = 'payload.issue.number'\nequals = 7"));
    assert!(holds("path = 'payload.labels[].name'\nequals = 'p1'"));
    assert!(holds(
        "path = 'payload.labels[].name'\nin = ['wontfix', 'bug']"
    ));
    assert!(!holds("path = 'payload.labels[].name'\nnot_equals = 'bug'"));
    assert!(holds("path = 'payload.labels[].name'\nnot_equals = 'docs'"));
    assert!(holds("path = 'payload.action'\nstarts_with = 'open'"));
    assert!(holds("path = 'payload.issue'\nexists = true"));
    assert!(holds("path = 'payload.pull_request'\nexists = false"));
    assert!(!holds("path = 'payload.labels[].color'\nexists = true"));
    let filter = [
        condition("path = 'payload.action'\nequals = 'opened'"),
        condition("path = 'payload.labels[].name'\nequals = 'docs'"),
    ];
    assert_eq!(
        failing(&filter, &input).map(ToString::to_string).as_deref(),
        Some(r#"payload.labels[].name equals "docs""#)
    );
    assert!(failing(&filter[..1], &input).is_none());
}

#[test]
fn a_condition_has_one_operator() {
    for toml in ["path = 'a'", "path = 'a'\nequals = 1\nexists = true"] {
        let condition: Condition = toml::from_str(toml).unwrap();
        assert!(condition.validate().is_err(), "{toml}");
    }
    assert!(toml::from_str::<Condition>("path = 'a'\nmatches = 'x'").is_err());
}
