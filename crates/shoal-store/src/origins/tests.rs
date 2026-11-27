use rusqlite::params;
use serde_json::{Value, json};
use shoal_protocol::sessions::{HostName, Origin};

use super::{OriginColumns, origin};

/// A Rex block Shoal created, with a zmx session nested in it.
fn nested_block() -> Origin {
    let mut zmx = Origin::created(HostName::Zmx, "chat-b559".into());
    zmx.owned = false;
    zmx.instance = Some("16435:1795721037".into());
    Origin {
        group: Some("session:01a1".into()),
        data: [("bin_dir".to_owned(), "/usr/bin".to_owned())].into(),
        instance: Some("2".into()),
        nested: Some(Box::new(zmx)),
        ..Origin::created(HostName::Rex, "block:01a2".into())
    }
}

#[test]
fn an_owned_place_gives_back_the_place_it_keeps_in_its_columns_and_json() -> anyhow::Result<()> {
    let db = crate::test_support::migrated()?;
    let columns = OriginColumns::of(&nested_block())?;
    db.execute(
        "INSERT INTO owned_places(origin_host,server,origin_place,origin_instance,space_id,origin)
         VALUES (?1,'',?2,?3,'space',?4)",
        params![
            columns.host,
            columns.place,
            columns.instance,
            columns.origin
        ],
    )?;

    let (read, kept): (Option<Origin>, String) = db.query_row(
        "SELECT origin_host,origin_place,origin_instance,origin FROM owned_places",
        [],
        |row| Ok((origin(row)?, row.get("origin")?)),
    )?;

    assert_eq!(read, Some(nested_block()));
    let kept: Value = serde_json::from_str(&kept)?;
    assert_eq!(
        kept,
        json!({
            "group": "session:01a1", "data": {"bin_dir": "/usr/bin"}, "owned": true,
            "nested": {"host": "zmx", "place": "chat-b559", "instance": "16435:1795721037"},
        })
    );
    Ok(())
}
