use serde_json::json;

use super::*;

#[test]
fn a_blocker_keeps_its_json_and_refuses_a_url_of_another_kind() {
    let pull = "https://forge.example/team/repo/pulls/8";
    let blocker: Blocker = serde_json::from_value(json!({"pr": pull})).unwrap();
    assert_eq!(serde_json::to_value(&blocker).unwrap(), json!({"pr": pull}));
    let issue: Blocker = serde_json::from_value(json!({"issue": "42"})).unwrap();
    assert_eq!(issue, Blocker::Issue(ItemInput::Number(42)));
    for mismatched in [
        json!({"pr": "https://forge.example/team/repo/issues/8"}),
        json!({"ci_run": pull}),
        json!({"issue": pull}),
    ] {
        assert!(serde_json::from_value::<Blocker>(mismatched).is_err());
    }
}

#[test]
fn a_blocker_is_an_issue_by_number_an_item_by_url_or_else_a_space() {
    let parsed = |input: &str| input.parse::<Blocker>().unwrap();
    assert_eq!(parsed("12"), Blocker::Issue("12".parse().unwrap()));
    let issue = "https://github.com/team/app/issues/7";
    assert_eq!(parsed(issue), Blocker::Issue(issue.parse().unwrap()));
    let pr = "https://github.com/team/app/pull/8";
    assert_eq!(parsed(pr), Blocker::Pr(pr.parse().unwrap()));
    let run = "https://github.com/team/app/actions/runs/9";
    assert_eq!(parsed(run), Blocker::CiRun(run.parse().unwrap()));
    assert_eq!(parsed("api"), Blocker::Space("api".into()));
    assert!(
        "https://github.com/team/app/tree/topic"
            .parse::<Blocker>()
            .is_err()
    );
}
