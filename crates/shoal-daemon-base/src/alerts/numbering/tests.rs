use super::*;

use crate::alerts::{About, AlertKind, NewAlert, insert};

/// A store in a temporary state directory.
async fn store() -> (tempfile::TempDir, crate::store::Store) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    (temp, store)
}

/// Record an alert for the user, returning its number.
fn record(tx: &Transaction<'_>) -> Result<i64> {
    let alert = NewAlert::shoal(AlertKind::Message, About::default(), "hello".into());
    insert(tx, None, &alert, false, false)
}

/// Keep `spares` as the hub gave them, with `taken` and `seen`.
fn numbered(tx: &Transaction<'_>, spares: &[i64], taken: i64, seen: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_alert_numbering(id, spares, taken, seen) VALUES (1, ?1, ?2, ?3)",
        params![serde_json::to_string(spares)?, taken, seen],
    )?;
    Ok(())
}

fn unconfirmed(tx: &Transaction<'_>) -> Result<Vec<i64>> {
    let mut statement = tx.prepare("SELECT number FROM hub_unconfirmed_alerts ORDER BY number")?;
    let numbers = statement.query_map([], |row| row.get(0))?;
    Ok(numbers.collect::<rusqlite::Result<_>>()?)
}

#[tokio::test]
async fn alerts_take_spares_in_order_then_wait_above_every_known_number() {
    let (_temp, store) = store().await;
    let (numbers, waiting) = store
        .write(|tx| {
            // Numbered alone, then by a hub whose spares one alert here
            // holds already.
            let alone = record(tx)?;
            numbered(tx, &[1, 9, 4], 0, 30)?;
            let numbers = [alone, record(tx)?, record(tx)?, record(tx)?];
            Ok((numbers, unconfirmed(tx)?))
        })
        .await
        .unwrap();
    // Past its spares, a machine numbers above the highest another
    // machine's alert took, which waits for the hub.
    assert_eq!(numbers, [1, 4, 9, 31]);
    assert_eq!(waiting, [31]);
}

#[tokio::test]
async fn a_number_taken_once_is_never_taken_again_here() {
    let (_temp, store) = store().await;
    let numbers = store
        .write(|tx| {
            numbered(tx, &[5, 6], 0, 0)?;
            let first = record(tx)?;
            tx.execute("DELETE FROM alerts WHERE id=?1", [first])?;
            Ok([first, record(tx)?])
        })
        .await
        .unwrap();
    assert_eq!(numbers, [5, 6]);
}

#[tokio::test]
async fn renumbering_gives_way_to_an_alert_that_holds_the_number() {
    let (_temp, store) = store().await;
    let renumbered = store
        .write(|tx| {
            let (first, second) = (record(tx)?, record(tx)?);
            Ok([renumber(tx, first, second)?, renumber(tx, first, 7)?])
        })
        .await
        .unwrap();
    assert_eq!(renumbered, [false, true]);
}
