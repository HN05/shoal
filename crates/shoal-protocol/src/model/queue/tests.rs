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
