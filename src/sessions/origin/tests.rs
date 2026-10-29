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
    assert!(serde_json::from_value::<Origin>(serde_json::json!({"host": "tmux"})).is_err());
    assert!("tmux".parse::<HostName>().is_err());
    assert_eq!("rex".parse::<HostName>().unwrap(), HostName::Rex);
}

#[test]
fn each_host_names_the_server_from_its_own_data() {
    let origin = |host, data: &[(&str, &str)]| Origin {
        data: data
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect(),
        ..Origin::created(host, "place".into())
    };
    let rex = origin(HostName::Rex, &[("server", "unix:///rex.sock")]);
    assert_eq!(rex.server(), "unix:///rex.sock");
    let herdr = origin(HostName::Herdr, &[("HERDR_SESSION", "work")]);
    assert_eq!(herdr.server(), "HERDR_SESSION=work");
    for host in [HostName::Zmx, HostName::Terminal] {
        assert_eq!(origin(host, &[("server", "unix:///rex.sock")]).server(), "");
    }
}
