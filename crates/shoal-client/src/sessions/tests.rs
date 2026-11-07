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
fn a_place_on_another_server_is_another_place() {
    let block = |server: &str| Origin {
        host: HostName::Rex,
        group: Some("session:1".into()),
        place: Some("block:2".into()),
        data: BTreeMap::from([("server".into(), server.into())]),
        owned: false,
        instance: None,
    };
    let here = block("unix:///a.sock");
    assert!(here.same_place(&Origin {
        owned: true,
        ..here.clone()
    }));
    assert!(!here.same_place(&block("unix:///b.sock")));
}
