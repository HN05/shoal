use serde_json::json;

use super::*;

fn desks() -> RepositoryElsewhere {
    serde_json::from_value(json!({
        "machine": "desk", "machine_id": "m-desk", "id": "r-desk", "name": "web",
        "origin": "forge/web",
    }))
    .unwrap()
}

#[tokio::test]
async fn a_picked_registration_acts_on_its_machine_alone_unless_others_are_named() {
    let (named, local, _, machines) = picked(&[], desks(), Machines::Found).await;
    assert_eq!(named, "r-desk");
    assert!(local.is_none());
    assert!(matches!(machines, Machines::Named(named) if named == ["m-desk"]));
    let (_, _, _, machines) = picked(&[], desks(), Machines::All).await;
    assert!(matches!(machines, Machines::All));
}
