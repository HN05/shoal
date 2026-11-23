use super::*;

#[test]
fn a_place_round_trips_without_its_empty_fields() {
    let origin = Origin {
        host: HostName::Rex,
        group: Some("session:1".into()),
        place: Some("block:2".into()),
        data: BTreeMap::from([("server".into(), "unix:///rex.sock".into())]),
        owned: true,
        instance: None,
        nested: Some(Box::new(Origin {
            instance: Some("41:1700000000".into()),
            ..Origin::created(HostName::Zmx, "work".into())
        })),
    };
    let json = serde_json::to_string(&origin).unwrap();
    assert_eq!(serde_json::from_str::<Origin>(&json).unwrap(), origin);
    let bare = serde_json::json!({"host": "zmx", "place": "work"});
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<Origin>(bare.clone()).unwrap()).unwrap(),
        bare
    );
}

#[test]
fn unknown_hosts_are_rejected() {
    assert!(serde_json::from_value::<Origin>(serde_json::json!({"host": "screen"})).is_err());
    assert!("screen".parse::<HostName>().is_err());
}

#[test]
fn a_place_lists_the_session_nested_in_it_after_itself() {
    let pane = Origin {
        nested: Some(Box::new(Origin::created(HostName::Zmx, "work".into()))),
        ..Origin::created(HostName::Herdr, "pane:1".into())
    };
    let places: Vec<_> = pane.with_nested().map(|place| place.host).collect();
    assert_eq!(places, [HostName::Herdr, HostName::Zmx]);
}
