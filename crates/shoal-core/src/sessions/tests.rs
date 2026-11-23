use super::*;

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

#[test]
fn a_place_is_named_by_host_server_id_and_instance_alone() {
    let block = |server: &str| Origin {
        data: [("server".into(), server.into())].into(),
        ..Origin::created(HostName::Rex, "block:2".into())
    };
    let here = block("unix:///a.sock");
    // A record from before Rex named blocks alone keeps its session, and
    // one from a `rex` elsewhere names another directory.
    let recorded = Origin {
        group: Some("session:1".into()),
        owned: false,
        data: [
            ("server".into(), "unix:///a.sock".into()),
            ("bin_dir".into(), "/old".into()),
        ]
        .into(),
        ..here.clone()
    };
    assert!(here.same_place(&recorded));
    assert!(!here.same_place(&block("unix:///b.sock")));
    assert!(!here.same_place(&Origin {
        instance: Some("2".into()),
        ..here.clone()
    }));
}
