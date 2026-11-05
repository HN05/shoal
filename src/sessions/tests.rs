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
