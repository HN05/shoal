use super::*;

#[test]
fn attachment_is_read_only_through_enabled_hosts() {
    let resolved = |global: &str| {
        let config = crate::config::Config::from_toml(global).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let (enabled, disabled) = (
        resolved(""),
        resolved("[terminal.herdr]\nenabled = false\n[terminal.rex]\nenabled = false\n"),
    );
    for host in [HostName::Herdr, HostName::Rex, HostName::Zmx] {
        let place = Origin::created(host, "p1".to_owned());
        assert!(
            Attachment::of(&enabled, place.clone()).is_some(),
            "{host:?}"
        );
        let read = Attachment::of(&disabled, place).is_some();
        assert_eq!(read, host == HostName::Zmx, "{host:?}");
    }
}

#[test]
fn a_session_label_names_its_agent() {
    let place = Origin::created(HostName::Zmx, "work".to_owned());
    let session = Session {
        agent: Some("claude".into()),
        ..Session::open("work".into(), 1, &place)
    };
    assert_eq!(session.label(), "work  claude  attached");
    assert_eq!(Session::open("work".into(), 0, &place).label(), "work");
    assert_eq!(
        serde_json::to_value(&session).unwrap()["agent"],
        serde_json::json!("claude")
    );
}
