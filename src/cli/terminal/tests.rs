use super::*;

#[test]
fn placement_follows_the_setting_then_whether_the_launch_waits() {
    assert_eq!(Placement::choose(None, true), Placement::Focus);
    assert_eq!(Placement::choose(None, false), Placement::Background);
    assert_eq!(Placement::choose(Some(false), true), Placement::Background);
    assert_eq!(Placement::choose(Some(true), false), Placement::Focus);
}

#[test]
fn a_place_repeats_only_in_the_same_host_and_server() {
    let pane = |socket: &str| Origin {
        data: [("HERDR_SOCKET_PATH".to_owned(), socket.to_owned())].into(),
        ..Origin::created(HostName::Herdr, "w1:p1".to_owned())
    };
    let (work, again, other) = (pane("/work.sock"), pane("/work.sock"), pane("/other.sock"));
    let rex = Origin::created(HostName::Rex, "w1:p1".to_owned());
    let distinct = distinct_places(&[&work, &again, &other, &rex]);
    assert_eq!(distinct, [&work, &other, &rex]);
}

#[test]
fn only_a_start_from_the_callers_place_splits_it_where_config_asks() {
    let resolved = |global: &str| {
        let config: crate::config::Config = toml::from_str(global).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let splits = resolved("[rex]\nplacement = 'split'\n");
    let windows = resolved("");
    let block = Origin {
        group: Some("session:1".into()),
        ..Origin::created(HostName::Rex, "block:1".into())
    };
    let session = Origin {
        place: None,
        ..block.clone()
    };
    let rex = |settings| Configured::named(settings, HostName::Rex);
    assert_eq!(rex(&splits).beside(Some(&block)).split, Some("block:1"));
    assert_eq!(rex(&splits).beside(Some(&session)).split, None);
    assert_eq!(rex(&splits).beside(None).split, None);
    assert_eq!(rex(&windows).beside(Some(&block)).split, None);
    let herdr = Configured::named(&splits, HostName::Herdr);
    assert_eq!(herdr.beside(Some(&block)).split, None);
}
