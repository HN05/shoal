use rusqlite::Connection;
use serde_json::json;

use super::*;

#[test]
fn a_kept_tree_reads_back_with_its_machines_and_parents() {
    let directory: Directory = serde_json::from_value(json!({
        "machines": [{
            "id": "laptop", "kind": "machine", "name": "laptop", "version": "1",
            "joined_at": 1, "last_seen": 2, "connected": true, "hub": false,
        }],
        "spaces": [{
            "machine": "laptop", "id": "child", "number": 4, "name": "child",
            "kind": "workspace", "parent": "coordinator", "status": "working",
            "reported_at": 3,
        }],
    }))
    .unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        include_str!("../../store/hub_routing.sql")
            .split("-- What a joined machine last learned")
            .nth(1)
            .map(|rest| format!("--{rest}"))
            .unwrap()
            .as_str(),
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let here = MemberId::from("desk".to_owned());
    store(&tx, &Known::of(&here, &directory)).unwrap();
    let (machine, kept) = load(&tx).unwrap().unwrap().directory();
    assert_eq!(machine, here);
    assert_eq!(kept.machines[0].name, "laptop");
    let child = &kept.spaces[0];
    assert_eq!(child.machine, MemberId::from("laptop".to_owned()));
    assert_eq!(
        child.space.parent,
        Some(SpaceId::from("coordinator".to_owned()))
    );
}
