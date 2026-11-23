use super::*;

/// `member`'s report of the spares it has `left` and the numbers its
/// alerts took `unconfirmed`, and the numbering the hub answers it with.
fn report(db: &mut Connection, member: &str, left: &[i64], unconfirmed: &[i64]) -> AlertNumbering {
    let tx = db.transaction().unwrap();
    let report = AlertsReport {
        spares: left.to_vec(),
        unconfirmed: unconfirmed.to_vec(),
        users: None,
    };
    let numbering = number(&tx, &MemberId::from(member.to_owned()), &report).unwrap();
    tx.commit().unwrap();
    numbering
}

fn renumbering(from: i64, to: i64) -> AlertRenumbering {
    AlertRenumbering { from, to }
}

#[test]
fn the_first_machine_keeps_its_numbers_and_the_next_takes_new_ones() {
    let mut db = shoal_store::test_support::migrated().unwrap();
    let desk = report(&mut db, "desk", &[], &[1, 2, 5]);
    assert_eq!(desk.confirmed, [1, 2, 5]);
    assert_eq!(desk.spares, (6..26).collect::<Vec<_>>());
    // A machine joining with alerts of its own renumbers every one below
    // the sequence, which may have named another machine's alert.
    let laptop = report(&mut db, "laptop", &[], &[3, 40]);
    assert_eq!(laptop.renumbered, [renumbering(3, 41)]);
    assert_eq!(laptop.confirmed, [40]);
    assert_eq!(laptop.spares, (42..62).collect::<Vec<_>>());
}

#[test]
fn a_number_taken_while_the_hub_was_out_of_reach_is_renumbered_once_another_machine_had_it() {
    let mut db = shoal_store::test_support::migrated().unwrap();
    let desk = report(&mut db, "desk", &[], &[]);
    let laptop = report(&mut db, "laptop", &[], &[]);
    assert_eq!((desk.spares[0], laptop.spares[0]), (1, 21));
    // The desk spends its spares and numbers above the highest it knows,
    // 20, which the laptop's spares hold.
    let desk = report(&mut db, "desk", &[], &[21, 22]);
    assert_eq!(desk.renumbered, [renumbering(21, 41), renumbering(22, 42)]);
    // Its answer lost, the desk reports them again, and they take numbers
    // never given before; once it heard, the hub keeps none for them.
    let again = report(&mut db, "desk", &desk.spares, &[21, 22]);
    assert_eq!(again.renumbered, [renumbering(21, 63), renumbering(22, 64)]);
    let heard = report(&mut db, "desk", &again.spares, &[]);
    assert_eq!((heard.confirmed.len(), heard.spares), (0, again.spares));
}

#[test]
fn spares_a_machine_used_or_never_heard_of_are_never_given_again() {
    let mut db = shoal_store::test_support::migrated().unwrap();
    let first = report(&mut db, "desk", &[], &[]);
    let used = &first.spares[..5];
    let left = &first.spares[5..];
    let second = report(&mut db, "desk", left, &[]);
    assert!(second.spares.iter().all(|spare| !used.contains(spare)));
    assert_eq!(second.spares.len(), SPARES);
    let laptop = report(&mut db, "laptop", &[], &[]);
    assert!(laptop.spares.iter().all(|spare| *spare > 25));
}

#[test]
fn a_renumbered_alert_goes_above_every_number_the_same_report_keeps() {
    let mut db = shoal_store::test_support::migrated().unwrap();
    let desk = report(&mut db, "desk", &[], &[]);
    let next = desk.spares.last().unwrap() + 1;
    let laptop = report(&mut db, "laptop", &[], &[3, next]);
    assert_eq!(laptop.confirmed, [next]);
    assert_eq!(laptop.renumbered, [renumbering(3, next + 1)]);
}
