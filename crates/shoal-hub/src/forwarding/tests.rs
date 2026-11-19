use serde_json::json;

use super::*;

#[test]
fn a_request_with_files_attached_here_is_one_that_attaches_files() {
    let method = |attachments: serde_json::Value| json!({"send_alert": {"to": {"space": {"space": "7"}}, "message": "hi", "attachments": attachments}});
    let attached = method(json!([{"path": "/tmp/report.md"}]));
    assert!(attaches_files(&attached));
    assert!(!attaches_files(&method(json!([]))));
    let rename = json!({"rename_space": {"space": "7", "name": "x"}});
    assert!(!attaches_files(&rename));
    // An issue's files sit within its content.
    let issue = json!({"open_issue": {"space": "7", "issue": {"title": "t",
        "attachments": [{"path": "/tmp/chart.png"}]}, "link": false}});
    assert!(attaches_files(&issue));
}

#[test]
fn a_forwarded_request_names_its_space_by_id() {
    let selector = SpaceSelector::Number(7.into());
    let id = SpaceId::from("space-seven".to_owned());
    let named = serde_json::to_value(&selector).unwrap();
    let mut method = json!({"send_alert": {"to": {"space": {"space": named}}, "message": "7"}});
    bind_space(&mut method, &selector, &id).unwrap();
    let bound = serde_json::to_value(SpaceSelector::id(&id)).unwrap();
    assert_eq!(method["send_alert"]["to"]["space"]["space"], bound);
    // Only what selects a space changes.
    assert_eq!(method["send_alert"]["message"], "7");
}

#[test]
fn binding_a_space_leaves_what_a_request_carries_alone() {
    use shoal_protocol::protocol::{CiMethod, ItemContext, Operation, RepoTarget};
    let selector = SpaceSelector::Number(7.into());
    let id = SpaceId::from("space-seven".to_owned());
    let context = ItemContext::Space {
        space: selector.clone(),
        repository: None,
    };
    let inputs = [("space".to_owned(), "7".to_owned())].into();
    let run = Operation::Ci(CiMethod::StartCiRun {
        target: RepoTarget::from(context),
        workflow: Some("ci.yml".into()),
        branch: None,
        inputs,
    });
    let mut method = serde_json::to_value(run).unwrap();
    bind_space(&mut method, &selector, &id).unwrap();
    let Operation::Ci(CiMethod::StartCiRun { target, inputs, .. }) =
        serde_json::from_value(method).unwrap()
    else {
        panic!("a CI run stays one");
    };
    let Some(ItemContext::Space { space, .. }) = target.context else {
        panic!("its target stays the space");
    };
    assert_eq!(space, SpaceSelector::id(&id));
    assert_eq!(inputs["space"], "7");
}
