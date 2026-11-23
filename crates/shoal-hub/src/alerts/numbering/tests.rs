use super::*;
use crate::stand_in::StandInRecords;

#[test]
fn a_renumbered_alert_leaves_no_lower_spare_for_a_later_one() {
    let mut db = shoal_store::test_support::migrated().unwrap();
    let tx = db.transaction().unwrap();
    tx.execute_batch(
        "INSERT INTO alerts(id,created_at,kind,sender,message) VALUES (6,0,'message','user','hi');
         INSERT INTO hub_alert_numbering(id,spares,taken,seen) VALUES (1,'[111,112]',0,0);
         INSERT INTO hub_unconfirmed_alerts(number) VALUES (6);",
    )
    .unwrap();
    let reported = reported(&tx).unwrap();
    let answer = AlertNumbering {
        spares: vec![111, 112],
        renumbered: vec![AlertRenumbering { from: 6, to: 125 }],
        ..AlertNumbering::default()
    };
    take(&tx, &StandInRecords, &reported, &answer).unwrap();
    let next = shoal_daemon_base::alerts::numbering::next(&tx).unwrap();
    assert_eq!(next, Some(126));
}
