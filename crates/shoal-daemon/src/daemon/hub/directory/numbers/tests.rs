use rusqlite::Connection;
use serde_json::json;
use shoal_protocol::hub::directory::{Report, SpaceSummary};

use super::*;

fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    for schema in [
        include_str!("../../../store/hub.sql"),
        include_str!("../../../store/hub_directory.sql"),
        include_str!("../../../store/hub_numbers.sql"),
    ] {
        db.execute_batch(schema).unwrap();
    }
    db.execute_batch(
        "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
         VALUES ('desk','machine','desk','k1',0), ('laptop','machine','laptop','k2',0);",
    )
    .unwrap();
    db
}

fn space(id: &str, number: u32) -> SpaceSummary {
    serde_json::from_value(json!({
        "id": id, "number": number, "name": id, "kind": "chat", "status": "working",
    }))
    .unwrap()
}

/// `member`'s report of `spaces` as they changed, and the numbering the
/// hub answers it with.
fn report(db: &mut Connection, member: &str, spaces: Vec<SpaceSummary>) -> Numbering {
    let member = MemberId::from(member.to_owned());
    let tx = db.transaction().unwrap();
    let changes = Report::Changes {
        changed: spaces,
        removed: Vec::new(),
    };
    super::super::records::apply(&tx, &member, changes).unwrap();
    let numbering = kept_for(&tx, &member).unwrap();
    tx.commit().unwrap();
    numbering
}

fn renumbering(space: &str, number: u32) -> Renumbering {
    Renumbering {
        space: SpaceId::from(space.to_owned()),
        number: SpaceNumber::new(number),
    }
}

fn numbers(numbers: &[u32]) -> Vec<SpaceNumber> {
    numbers.iter().copied().map(SpaceNumber::new).collect()
}

#[test]
fn a_machine_keeps_spares_no_space_holds() {
    let mut db = database();
    let desk = report(&mut db, "desk", vec![space("a", 1), space("b", 3)]);
    assert_eq!(desk.spares, numbers(&[2, 4, 5]));
    assert_eq!(desk.renumbered, []);
    let laptop = report(&mut db, "laptop", Vec::new());
    assert_eq!(laptop.spares, numbers(&[6, 7, 8]));
    // A spare the machine numbered a space with is the space's, and the
    // hub keeps another.
    let desk = report(&mut db, "desk", vec![space("c", 2)]);
    assert_eq!(desk.spares, numbers(&[4, 5, 9]));
}

#[test]
fn a_space_whose_number_another_machine_held_first_is_renumbered() {
    let mut db = database();
    report(&mut db, "desk", vec![space("a", 1), space("b", 2)]);
    let laptop = report(&mut db, "laptop", vec![space("c", 2), space("d", 7)]);
    assert_eq!(laptop.renumbered, [renumbering("c", 6)]);
    // The space that held the number first keeps it, and so does the one
    // renumbered until it reports its new number.
    let desk = report(&mut db, "desk", vec![space("b", 2)]);
    assert_eq!(desk.renumbered, []);
    let laptop = report(&mut db, "laptop", vec![space("c", 2)]);
    assert_eq!(laptop.renumbered, [renumbering("c", 6)]);
    let laptop = report(&mut db, "laptop", vec![space("c", 6)]);
    assert_eq!(laptop.renumbered, []);
}

#[test]
fn a_space_with_another_machines_spare_is_renumbered() {
    let mut db = database();
    let desk = report(&mut db, "desk", Vec::new());
    assert_eq!(desk.spares, numbers(&[1, 2, 3]));
    // Numbered above its own highest while it could not reach the hub.
    let laptop = report(&mut db, "laptop", vec![space("a", 1)]);
    assert_eq!(laptop.renumbered, [renumbering("a", 4)]);
    let desk = report(&mut db, "desk", vec![space("b", 1)]);
    assert_eq!(
        (desk.spares, desk.renumbered),
        (numbers(&[2, 3, 8]), vec![])
    );
}

#[test]
fn a_space_that_ends_frees_the_number_kept_for_it() {
    let mut db = database();
    report(&mut db, "desk", vec![space("a", 1)]);
    report(&mut db, "laptop", vec![space("b", 1)]);
    let tx = db.transaction().unwrap();
    let removed = Report::Changes {
        changed: Vec::new(),
        removed: vec![SpaceId::from("b".to_owned())],
    };
    let laptop = MemberId::from("laptop".to_owned());
    super::super::records::apply(&tx, &laptop, removed).unwrap();
    assert_eq!(smallest_free(&tx).unwrap(), SpaceNumber::new(5));
    tx.commit().unwrap();
    let laptop = report(&mut db, "laptop", Vec::new());
    assert_eq!(laptop.renumbered, []);
}
