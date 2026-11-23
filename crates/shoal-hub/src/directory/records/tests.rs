use rusqlite::Connection;
use serde_json::json;

use super::*;

fn database() -> Connection {
    let db = shoal_store::test_support::migrated().unwrap();
    db.execute_batch(
        "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
         VALUES ('desk','machine','desk','k1',0), ('laptop','machine','laptop','k2',0);",
    )
    .unwrap();
    db
}

fn space(id: &str, name: &str) -> SpaceSummary {
    serde_json::from_value(json!({
        "id": id, "number": 1, "name": name, "kind": "chat", "status": "working",
    }))
    .unwrap()
}

fn report(db: &mut Connection, member: &str, report: Report) -> Vec<Conflict> {
    let tx = db.transaction().unwrap();
    let conflicts = apply(&tx, &MemberId::from(member.to_owned()), report).unwrap();
    tx.commit().unwrap();
    conflicts
}

/// Each listed space's ID with the machine that owns it.
fn listed(db: &mut Connection) -> Vec<(String, String)> {
    let tx = db.transaction().unwrap();
    let (spaces, _) = read(&tx).unwrap();
    spaces
        .into_iter()
        .map(|listed| (listed.space.id.to_string(), listed.machine.to_string()))
        .collect()
}

#[test]
fn a_whole_report_replaces_what_the_machine_had() {
    let mut db = database();
    report(
        &mut db,
        "desk",
        Report::Whole(vec![space("a", "one"), space("b", "two")]),
    );
    report(&mut db, "desk", Report::Whole(vec![space("b", "two")]));
    assert_eq!(listed(&mut db), [("b".into(), "desk".into())]);
    let removed = vec![SpaceId::from("b".to_owned())];
    let changes = Report::Changes {
        changed: vec![space("c", "three")],
        removed,
    };
    report(&mut db, "desk", changes);
    assert_eq!(listed(&mut db), [("c".into(), "desk".into())]);
}

#[test]
fn a_second_machine_reporting_an_id_raises_one_conflict_and_is_left_out() {
    let mut db = database();
    report(&mut db, "desk", Report::Whole(vec![space("a", "one")]));
    let conflicts = report(&mut db, "laptop", Report::Whole(vec![space("a", "copy")]));
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].owner.as_str(), "desk");
    assert_eq!(conflicts[0].copy.as_str(), "laptop");
    let again = report(&mut db, "laptop", Report::Whole(vec![space("a", "copy")]));
    assert!(again.is_empty());
    assert_eq!(listed(&mut db), [("a".into(), "desk".into())]);
    let tx = db.transaction().unwrap();
    let (_, kept) = read(&tx).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name.as_str(), "copy");
}

#[test]
fn the_earliest_copy_owns_an_id_its_owner_released() {
    let mut db = database();
    report(&mut db, "desk", Report::Whole(vec![space("a", "one")]));
    report(&mut db, "laptop", Report::Whole(vec![space("a", "moved")]));
    report(&mut db, "desk", Report::Whole(Vec::new()));
    assert_eq!(listed(&mut db), [("a".into(), "laptop".into())]);
    let tx = db.transaction().unwrap();
    assert!(read(&tx).unwrap().1.is_empty());
}

#[test]
fn a_forgotten_machine_hands_its_ids_to_their_copies() {
    let mut db = database();
    report(
        &mut db,
        "desk",
        Report::Whole(vec![space("a", "one"), space("b", "two")]),
    );
    report(&mut db, "laptop", Report::Whole(vec![space("a", "copy")]));
    let tx = db.transaction().unwrap();
    forget(&tx, &MemberId::from("desk".to_owned())).unwrap();
    tx.commit().unwrap();
    assert_eq!(listed(&mut db), [("a".into(), "laptop".into())]);
}

#[test]
fn a_claimed_space_changes_hands_without_a_conflict() {
    let mut db = database();
    report(&mut db, "desk", Report::Whole(vec![space("a", "moving")]));
    let tx = db.transaction().unwrap();
    claim(
        &tx,
        &MemberId::from("laptop".to_owned()),
        &"a".to_owned().into(),
    )
    .unwrap();
    tx.commit().unwrap();
    let conflicts = report(&mut db, "laptop", Report::Whole(vec![space("a", "moving")]));
    assert!(conflicts.is_empty());
    assert_eq!(listed(&mut db), [("a".into(), "laptop".into())]);
    // The machine it moved from reports it until its space ends there.
    let conflicts = report(&mut db, "desk", Report::Whole(vec![space("a", "moving")]));
    assert!(conflicts.is_empty());
    let tx = db.transaction().unwrap();
    assert!(read(&tx).unwrap().1.is_empty());
}

/// Have desk report the space `a`, then laptop claim and report it.
fn moved_to_laptop(db: &mut Connection) {
    report(db, "desk", Report::Whole(vec![space("a", "moving")]));
    let tx = db.transaction().unwrap();
    claim(
        &tx,
        &MemberId::from("laptop".to_owned()),
        &"a".to_owned().into(),
    )
    .unwrap();
    tx.commit().unwrap();
    report(db, "laptop", Report::Whole(vec![space("a", "moving")]));
}

#[test]
fn a_moved_space_its_new_machine_ended_has_no_owner_before_the_old_one_ends_it() {
    let mut db = database();
    moved_to_laptop(&mut db);
    report(&mut db, "laptop", Report::Whole(Vec::new()));
    assert!(listed(&mut db).is_empty());
    // Desk reports its space again as it ends it.
    report(&mut db, "desk", Report::Whole(vec![space("a", "ending")]));
    let tx = db.transaction().unwrap();
    let laptop = MemberId::from("laptop".to_owned());
    assert_eq!(
        owner_seen_by(&tx, &"a".to_owned().into(), &laptop).unwrap(),
        None
    );
}

#[test]
fn a_moved_space_its_new_machine_restored_is_its_own() {
    let mut db = database();
    moved_to_laptop(&mut db);
    report(&mut db, "laptop", Report::Whole(Vec::new()));
    let conflicts = report(
        &mut db,
        "laptop",
        Report::Whole(vec![space("a", "restored")]),
    );
    assert!(conflicts.is_empty());
    report(&mut db, "desk", Report::Whole(Vec::new()));
    assert_eq!(listed(&mut db), [("a".into(), "laptop".into())]);
}

#[test]
fn a_machine_that_claims_a_space_sees_no_other_owner() {
    let mut db = database();
    report(&mut db, "desk", Report::Whole(vec![space("a", "moving")]));
    let (desk, laptop) = (
        MemberId::from("desk".to_owned()),
        MemberId::from("laptop".to_owned()),
    );
    let id = SpaceId::from("a".to_owned());
    let tx = db.transaction().unwrap();
    assert_eq!(
        owner_seen_by(&tx, &id, &laptop).unwrap(),
        Some(desk.clone())
    );
    claim(&tx, &laptop, &id).unwrap();
    assert_eq!(owner_seen_by(&tx, &id, &laptop).unwrap(), None);
    assert_eq!(owner_seen_by(&tx, &id, &desk).unwrap(), Some(desk));
}

#[test]
fn a_claim_is_spent_when_its_space_ended_at_the_source_first() {
    let mut db = database();
    let (desk, laptop) = (
        MemberId::from("desk".to_owned()),
        MemberId::from("laptop".to_owned()),
    );
    let id = SpaceId::from("a".to_owned());
    report(&mut db, "desk", Report::Whole(vec![space("a", "moving")]));
    let tx = db.transaction().unwrap();
    claim(&tx, &laptop, &id).unwrap();
    tx.commit().unwrap();
    report(&mut db, "desk", Report::Whole(Vec::new()));
    report(&mut db, "laptop", Report::Whole(vec![space("a", "moving")]));
    // It moves back; the claim it spent no longer hides desk as its owner.
    report(&mut db, "laptop", Report::Whole(Vec::new()));
    report(&mut db, "desk", Report::Whole(vec![space("a", "back")]));
    let tx = db.transaction().unwrap();
    assert_eq!(owner_seen_by(&tx, &id, &laptop).unwrap(), Some(desk));
}

#[test]
fn a_space_that_moves_back_before_its_copy_ends_conflicts_with_no_one() {
    let mut db = database();
    let (desk, laptop) = (
        MemberId::from("desk".to_owned()),
        MemberId::from("laptop".to_owned()),
    );
    let id = SpaceId::from("a".to_owned());
    let claim_by = |db: &mut Connection, member: &MemberId| {
        let tx = db.transaction().unwrap();
        claim(&tx, member, &id).unwrap();
        tx.commit().unwrap();
    };
    report(&mut db, "desk", Report::Whole(vec![space("a", "moving")]));
    claim_by(&mut db, &laptop);
    report(&mut db, "laptop", Report::Whole(vec![space("a", "moving")]));
    // Back to desk before desk reported that its first copy ended.
    claim_by(&mut db, &desk);
    report(&mut db, "desk", Report::Whole(vec![space("a", "back")]));
    assert_eq!(listed(&mut db), [("a".into(), "desk".into())]);
    let tx = db.transaction().unwrap();
    let (_, conflicts) = read(&tx).unwrap();
    assert!(conflicts.iter().all(|conflict| conflict.copy != desk));
}

#[test]
fn a_revoked_machine_records_nothing() {
    let mut db = database();
    db.execute("UPDATE hub_members SET revoked_at=1 WHERE id='desk'", [])
        .unwrap();
    let tx = db.transaction().unwrap();
    let desk = MemberId::from("desk".to_owned());
    assert!(apply(&tx, &desk, Report::Whole(vec![space("a", "late")])).is_err());
    assert!(claim(&tx, &desk, &"b".to_owned().into()).is_err());
    drop(tx);
    assert!(listed(&mut db).is_empty());
}

#[test]
fn a_returning_machine_s_copy_of_a_space_taken_from_it_raises_no_conflict() {
    let mut db = database();
    report(&mut db, "desk", Report::Whole(vec![space("a", "taken")]));
    db.execute(
        "INSERT INTO hub_takeovers(space_id, member_id, taken_at) VALUES ('a','laptop',0)",
        [],
    )
    .unwrap();
    let conflicts = report(&mut db, "laptop", Report::Whole(vec![space("a", "taken")]));
    assert!(conflicts.is_empty());
    assert_eq!(listed(&mut db), [("a".into(), "desk".into())]);
}

/// Each removed space a machine keeps something of, with that machine and
/// what it keeps.
fn kept_removed(db: &mut Connection) -> Vec<(String, String, Keeps)> {
    let tx = db.transaction().unwrap();
    (kept(&tx).unwrap().into_iter())
        .map(|kept| (kept.space.to_string(), kept.machine.to_string(), kept.keeps))
        .collect()
}

fn report_kept(db: &mut Connection, member: &str, records: &[&str], files: &[&str]) {
    let ids = |ids: &[&str]| {
        ids.iter()
            .map(|id| SpaceId::from((*id).to_owned()))
            .collect()
    };
    let kept = KeptRemoved {
        records: ids(records),
        files: ids(files),
    };
    let tx = db.transaction().unwrap();
    keep(&tx, &MemberId::from(member.to_owned()), &kept).unwrap();
    tx.commit().unwrap();
}

#[test]
fn what_a_machine_keeps_of_removed_spaces_replaces_its_last_report_until_it_is_forgotten() {
    let mut db = database();
    report_kept(&mut db, "desk", &["a"], &["a", "b"]);
    report_kept(&mut db, "laptop", &["c"], &[]);
    report_kept(&mut db, "desk", &["b"], &["b"]);
    let expected = [
        ("b".into(), "desk".into(), Keeps::Files),
        ("b".into(), "desk".into(), Keeps::Record),
        ("c".into(), "laptop".into(), Keeps::Record),
    ];
    assert_eq!(kept_removed(&mut db), expected);
    let tx = db.transaction().unwrap();
    forget(&tx, &MemberId::from("laptop".to_owned())).unwrap();
    tx.commit().unwrap();
    assert_eq!(kept_removed(&mut db), expected[..2]);
}
