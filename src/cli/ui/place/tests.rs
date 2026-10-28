use super::*;

fn entries() -> Entries {
    vec![
        ("api".into(), "api   queued, position 1".into()),
        ("ui".into(), "ui    queued, position 2".into()),
    ]
}

#[test]
fn places_are_in_front_of_an_entry_or_last() {
    let entries = entries();
    let rows = place_rows(&entries);
    let ids: Vec<_> = rows.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["0", "1", "last"]);
    assert_eq!(rows[2].1, "Last");
    assert_eq!(place_of(&entries, "1").unwrap(), Place::Before("ui".into()));
    assert_eq!(place_of(&entries, "last").unwrap(), Place::Last);
    assert!(place_of(&entries, "2").is_err());
}
