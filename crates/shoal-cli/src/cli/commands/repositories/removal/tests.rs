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
    let found = picked(&[], desks(), Machines::Found).await;
    assert_eq!(found.named, "r-desk");
    assert!(found.local.is_none());
    assert!(matches!(found.machines, Machines::Named(named) if named == ["m-desk"]));
    let all = picked(&[], desks(), Machines::All).await;
    assert!(matches!(all.machines, Machines::All));
}
